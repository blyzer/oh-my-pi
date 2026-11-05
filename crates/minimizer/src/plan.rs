//! Structural analysis of a shell command using the engine's own parser.
//!
//! The minimizer must not corrupt downstream parsing or stitch together
//! segments that emit interleaved output. This module parses the full
//! command with [`omp_shell::parser`], the grammar the omp shell engine
//! executes, and classifies it into one of a few shapes the engine can reason
//! about. Segments therefore split exactly where the executing engine splits
//! the command.
//!
//! ## Decisions encoded here
//!
//! - **Pipes are opaque.** Any `foo | bar` pipeline is marked as `Piped`
//!   regardless of what `bar` is. A user piping through `awk`, `jq`, `rg`, or
//!   any other consumer is almost certainly parsing the output; rewriting it
//!   would be a correctness bug. The engine falls back to passthrough.
//! - **Safe chains are segmented, not rewritten whole.** Top-level simple
//!   commands joined only by `&&` and `;` may be split into `ChainSegment`s for
//!   the segmented engine path, but the whole-buffer minimizer still treats the
//!   combined chain as opaque. Each segment's text is the verbatim source span
//!   of its pipeline, redirections included, so quoting and redirect spelling
//!   survive exactly as written. Source offsets and lengths retain that same
//!   pipeline span in original-command bytes; execution wrappers are the
//!   caller's responsibility.
//! - **Other compound commands are opaque.** `a || b`, background jobs, and
//!   compound shell syntax such as subshells or function definitions are left
//!   unchanged. Here-documents, command/process substitutions and arithmetic
//!   expansions keep a command out of segmentation as well.
//! - **Single simple commands** are safe for the whole-buffer path; the engine
//!   dispatches them through `detect.rs` as before.
//!
//! When the command fails to parse (syntax error, unsupported construct),
//! we return `Unsupported` and the engine passes through.

use std::iter;

use omp_shell::parser::{
	ParserOptions, Token, ast, parse_tokens, uncached_tokenize_str,
	word::{self, Parameter, ParameterExpr, WordPiece, WordPieceWithSource},
};

/// One segment of a safe `&&` / `;` chain.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChainSegment {
	/// The segment's pipeline verbatim from the source, redirections included.
	pub command:                   String,
	/// First byte of the pipeline in the original command, before any run
	/// wrapper.
	pub source_at:                 u32,
	/// Byte length of the pipeline in the original command, excluding run
	/// wrappers.
	pub source_len:                u32,
	/// Leading command word of the segment's first stage, as written.
	pub program:                   String,
	/// The segment follows `&&`: it runs only when the previous one succeeded.
	pub run_if_previous_succeeded: bool,
	/// The segment is followed by `&&`, so its failure must not trip `errexit`.
	pub suppress_errexit:          bool,
}

/// Outcome of analyzing a raw command string.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CommandPlan {
	/// Exactly one simple command. `program` is the leading word (without
	/// arguments), verbatim from the source.
	Single {
		/// Leading command word as written.
		program: String,
	},
	/// The command contains at least one `|` pipeline. We intentionally do
	/// NOT identify upstream / downstream programs here — any pipe defeats
	/// safe minimization for this engine.
	Piped,
	/// Top-level simple commands joined by `&&` and/or `;`. These can be
	/// minimized segment-by-segment, but not as one combined buffer.
	Chain {
		/// The segments in execution order.
		segments: Vec<ChainSegment>,
	},
	/// The command has multiple segments joined by `||`, `&`, or other
	/// unsupported shell syntax. This shape is left unchanged; the minimizer
	/// only rewrites whole simple command output.
	Compound,
	/// Parse failed, a compound shell construct (for loops, subshells, etc.)
	/// was encountered, or the command was empty.
	Unsupported,
}

/// Parse `command` with the engine's bash parser and classify its structure.
#[must_use]
pub fn analyze(command: &str) -> CommandPlan {
	if command.trim().is_empty() {
		return CommandPlan::Unsupported;
	}
	let Some((tokens, program)) = parse(command) else {
		return CommandPlan::Unsupported;
	};
	classify(&program, &tokens, command)
}

/// Parse `command` with the grammar a fresh engine shell runs it with
/// (default parser options), discarding error detail. The tokens come back
/// too: the tree records no source location for redirections, so segment
/// spans are read off the token stream. Returns `None` on any syntax error.
fn parse(command: &str) -> Option<(Vec<Token>, ast::Program)> {
	let options = ParserOptions::default();
	let tokens = uncached_tokenize_str(command, &options.tokenizer_options()).ok()?;
	let program = parse_tokens(&tokens, &options).ok()?;
	Some((tokens, program))
}

/// Byte offset of the `index`-th char of `src`: parser positions count chars.
fn byte_offset(src: &str, index: usize) -> Option<usize> {
	src.char_indices()
		.map(|(at, _)| at)
		.chain(iter::once(src.len()))
		.nth(index)
}

/// `&` backgrounds the item: the parent's stdout is the foreground command's,
/// so we cannot tell which output we capture.
const fn is_async(sep: &ast::SeparatorOperator) -> bool {
	matches!(sep, ast::SeparatorOperator::Async)
}

fn classify(program: &ast::Program, tokens: &[Token], src: &str) -> CommandPlan {
	// Separator-separated top-level items across all complete commands.
	let items: Vec<&ast::CompoundListItem> = program
		.complete_commands
		.iter()
		.flat_map(|complete| complete.0.iter())
		.collect();

	if let Some(chain) = classify_chain(&items, tokens, src) {
		return chain;
	}

	let [item] = items.as_slice() else {
		// `a ; b` or `a & b` produces multiple list items.
		return if items.is_empty() {
			CommandPlan::Unsupported
		} else {
			CommandPlan::Compound
		};
	};

	// Conservative bail on a backgrounded command, and on `&&` / `||`
	// continuations that did not form a safe chain.
	if is_async(&item.1) || !item.0.additional.is_empty() {
		return CommandPlan::Compound;
	}

	// Only a single pipeline at this point.
	classify_pipeline(&item.0.first, src).unwrap_or(CommandPlan::Unsupported)
}

fn classify_chain(
	items: &[&ast::CompoundListItem],
	tokens: &[Token],
	src: &str,
) -> Option<CommandPlan> {
	// (program, run_if_previous_succeeded, suppress_errexit) per pipeline.
	let mut pipelines: Vec<(String, bool, bool)> = Vec::new();

	for item in items {
		if is_async(&item.1) {
			return None;
		}

		let mut run_if_previous_succeeded = false;
		let mut pipeline = &item.0.first;
		let mut rest = item.0.additional.iter().peekable();

		loop {
			let program = simple_segment(pipeline, src)?;

			let suppress_errexit = rest
				.peek()
				.is_some_and(|next| matches!(next, ast::AndOr::And(_)));
			pipelines.push((program, run_if_previous_succeeded, suppress_errexit));

			match rest.next() {
				None => break,
				Some(ast::AndOr::And(next_pipeline)) => {
					run_if_previous_succeeded = true;
					pipeline = next_pipeline;
				},
				Some(ast::AndOr::Or(_)) => return None,
			}
		}
	}

	if pipelines.len() < 2 {
		return None;
	}

	// The chain runner re-executes each segment's text: take it verbatim
	// from the source (all stages and their redirections) rather than
	// re-printing the tree, so it is exactly what the user wrote. A token
	// split that disagrees with the tree fails closed.
	let spans = pipeline_spans(tokens);
	if spans.len() != pipelines.len() {
		return None;
	}
	let segments = pipelines
		.into_iter()
		.zip(spans)
		.map(|((program, run_if_previous_succeeded, suppress_errexit), (start, end))| {
			let source_at = byte_offset(src, start)?;
			let source_end = byte_offset(src, end)?;
			let command = src.get(source_at..source_end)?;
			if command.trim().is_empty() {
				return None;
			}
			Some(ChainSegment {
				command: command.to_owned(),
				source_at: u32::try_from(source_at).ok()?,
				source_len: u32::try_from(source_end - source_at).ok()?,
				program,
				run_if_previous_succeeded,
				suppress_errexit,
			})
		})
		.collect::<Option<Vec<_>>>()?;

	Some(CommandPlan::Chain { segments })
}

/// Char spans of the top-level pipelines, in order: the token runs between
/// list operators. Only meaningful for a flat chain of simple commands; a
/// newline after `|` or `|&` continues the pipeline.
fn pipeline_spans(tokens: &[Token]) -> Vec<(usize, usize)> {
	let mut spans = Vec::new();
	let mut current: Option<(usize, usize)> = None;
	let mut after_pipe = false;
	for token in tokens {
		if let Token::Operator(operator, _) = token {
			match operator.as_str() {
				"\n" if after_pipe => continue,
				"&&" | "||" | ";" | "&" | "\n" => {
					spans.extend(current.take());
					continue;
				},
				operator => after_pipe = matches!(operator, "|" | "|&"),
			}
		} else {
			after_pipe = false;
		}
		let span = token.location();
		let start = current.map_or(span.start.index, |(start, _)| start);
		current = Some((start, span.end.index));
	}
	spans.extend(current);
	spans
}

/// Whether shell word text expands without running anything: no command
/// substitution or arithmetic expansion at any depth. Text the word parser
/// rejects is unsafe.
fn text_is_safe(text: &str) -> bool {
	word::parse(text, &ParserOptions::default()).is_ok_and(|pieces| pieces_are_safe(text, &pieces))
}

fn word_is_safe(word: &ast::Word) -> bool {
	text_is_safe(&word.value)
}

/// `text` is the word the pieces were parsed from; piece indices are byte
/// offsets into it.
fn pieces_are_safe(text: &str, pieces: &[WordPieceWithSource]) -> bool {
	pieces.iter().all(|piece| match &piece.piece {
		// A `$` the parser kept literal before `{` is a `${…}` it could not
		// parse: a bad substitution, or bash 5.3's `${ …;}` command
		// substitution. Either way it stays with the whole command.
		WordPiece::Text(literal) => {
			!(literal.ends_with('$')
				&& text
					.get(piece.end_index..)
					.is_some_and(|rest| rest.starts_with('{')))
		},
		WordPiece::SingleQuotedText(_)
		| WordPiece::AnsiCQuotedText(_)
		| WordPiece::EscapeSequence(_)
		| WordPiece::TildeExpansion(_) => true,
		WordPiece::DoubleQuotedSequence(inner) | WordPiece::GettextDoubleQuotedSequence(inner) => {
			pieces_are_safe(text, inner)
		},
		WordPiece::ParameterExpansion(expr) => param_is_safe(expr),
		// Command substitutions (`$(…)`, backticks) run commands of their
		// own. Arithmetic text is not parsed into the tree and may hide one.
		// Both stay with the unsegmented whole command.
		WordPiece::CommandSubstitution(_)
		| WordPiece::BackquotedCommandSubstitution(_)
		| WordPiece::ArithmeticExpression(_) => false,
	})
}

/// Arithmetic text inside `${x:off:len}` is unparsed; any `$` or backquote in
/// it may be a command substitution, so only expansion-free text is safe.
fn arith_text_is_safe(text: &str) -> bool {
	!text.bytes().any(|byte| matches!(byte, b'$' | b'`'))
}

fn parameter_is_safe(parameter: &Parameter) -> bool {
	match parameter {
		Parameter::NamedWithIndex { index, .. } => text_is_safe(index),
		Parameter::Positional(_)
		| Parameter::Special(_)
		| Parameter::Named(_)
		| Parameter::NamedWithAllIndices { .. } => true,
	}
}

fn param_is_safe(expr: &ParameterExpr) -> bool {
	match expr {
		ParameterExpr::Parameter { parameter, .. }
		| ParameterExpr::ParameterLength { parameter, .. }
		| ParameterExpr::Transform { parameter, .. } => parameter_is_safe(parameter),
		ParameterExpr::UseDefaultValues { parameter, default_value: word, .. }
		| ParameterExpr::AssignDefaultValues { parameter, default_value: word, .. }
		| ParameterExpr::IndicateErrorIfNullOrUnset { parameter, error_message: word, .. }
		| ParameterExpr::UseAlternativeValue { parameter, alternative_value: word, .. }
		| ParameterExpr::RemoveSmallestSuffixPattern { parameter, pattern: word, .. }
		| ParameterExpr::RemoveLargestSuffixPattern { parameter, pattern: word, .. }
		| ParameterExpr::RemoveSmallestPrefixPattern { parameter, pattern: word, .. }
		| ParameterExpr::RemoveLargestPrefixPattern { parameter, pattern: word, .. }
		| ParameterExpr::UppercaseFirstChar { parameter, pattern: word, .. }
		| ParameterExpr::UppercasePattern { parameter, pattern: word, .. }
		| ParameterExpr::LowercaseFirstChar { parameter, pattern: word, .. }
		| ParameterExpr::LowercasePattern { parameter, pattern: word, .. } => {
			parameter_is_safe(parameter) && word.as_deref().is_none_or(text_is_safe)
		},
		ParameterExpr::Substring { parameter, offset, length, .. } => {
			parameter_is_safe(parameter)
				&& arith_text_is_safe(&offset.value)
				&& length
					.as_ref()
					.is_none_or(|length| arith_text_is_safe(&length.value))
		},
		ParameterExpr::ReplaceSubstring { parameter, pattern, replacement, .. } => {
			parameter_is_safe(parameter)
				&& text_is_safe(pattern)
				&& replacement.as_deref().is_none_or(text_is_safe)
		},
		ParameterExpr::VariableNames { .. } | ParameterExpr::MemberKeys { .. } => true,
	}
}

fn assign_is_safe(assignment: &ast::Assignment) -> bool {
	let index_is_safe = match &assignment.name {
		ast::AssignmentName::VariableName(_) => true,
		ast::AssignmentName::ArrayElementName(_, index) => text_is_safe(index),
	};
	index_is_safe
		&& match &assignment.value {
			ast::AssignmentValue::Scalar(word) => word_is_safe(word),
			ast::AssignmentValue::Array(elements) => elements
				.iter()
				.all(|(key, value)| key.as_ref().is_none_or(word_is_safe) && word_is_safe(value)),
		}
}

fn redir_is_safe(redirect: &ast::IoRedirect) -> bool {
	match redirect {
		ast::IoRedirect::File(_, _, target) => match target {
			ast::IoFileRedirectTarget::Filename(word) | ast::IoFileRedirectTarget::Duplicate(word) => {
				word_is_safe(word)
			},
			ast::IoFileRedirectTarget::Fd(_) => true,
			// `> >(cat)` runs a command of its own.
			ast::IoFileRedirectTarget::ProcessSubstitution(..) => false,
		},
		ast::IoRedirect::HereString(_, word) | ast::IoRedirect::OutputAndError(word, _) => {
			word_is_safe(word)
		},
		// Here-docs are never safe to segment: the document body follows the
		// command's line, outside the pipeline's source span, so a segment
		// sliced from that span would run without its body ("unterminated
		// here document"). Leave any here-doc-bearing command to the
		// unsegmented single path.
		ast::IoRedirect::HereDocument(..) => false,
	}
}

fn item_is_safe(item: &ast::CommandPrefixOrSuffixItem) -> bool {
	match item {
		ast::CommandPrefixOrSuffixItem::Word(word) => word_is_safe(word),
		ast::CommandPrefixOrSuffixItem::AssignmentWord(assignment, _) => assign_is_safe(assignment),
		ast::CommandPrefixOrSuffixItem::IoRedirect(redirect) => redir_is_safe(redirect),
		// `<(…)` runs a command of its own.
		ast::CommandPrefixOrSuffixItem::ProcessSubstitution(..) => false,
	}
}

/// The simple command `command` is, when every part of it is safe to run as a
/// standalone segment: no command/process substitution, arithmetic expansion
/// or here-doc in its assignments, words, or redirections.
fn safe_simple(command: &ast::Command) -> Option<&ast::SimpleCommand> {
	let ast::Command::Simple(simple) = command else {
		return None;
	};
	let mut items = simple
		.prefix
		.iter()
		.flat_map(|prefix| &prefix.0)
		.chain(simple.suffix.iter().flat_map(|suffix| &suffix.0));
	let safe = simple.word_or_name.as_ref().is_none_or(word_is_safe) && items.all(item_is_safe);
	safe.then_some(simple)
}

/// Leading command word of `simple`, verbatim from the source.
fn program_word<'s>(simple: &ast::SimpleCommand, src: &'s str) -> Option<&'s str> {
	let span = simple.word_or_name.as_ref()?.loc.as_ref()?;
	let start = byte_offset(src, span.start.index)?;
	let end = byte_offset(src, span.end.index)?;
	let program = src.get(start..end)?;
	(!program.trim().is_empty()).then_some(program)
}

fn simple_segment(pipeline: &ast::Pipeline, src: &str) -> Option<String> {
	if pipeline.timed.is_some() || pipeline.bang {
		return None;
	}

	// Every stage must be a safe simple command. Compound stages
	// (`if` / `for` / `while` / subshells / `{ … }`) keep the whole chain
	// unsegmented; validating only the first stage once let
	// `git log … | while read x; do … done` through.
	let mut stages = pipeline.seq.iter();
	let first = safe_simple(stages.next()?)?;
	if !stages.all(|stage| safe_simple(stage).is_some()) {
		return None;
	}

	// Identify the segment by its first stage's program word. Multi-stage pipes
	// are captured but never rewritten (runtime detects `CommandPlan::Piped`),
	// keeping the chain decomposable when an inner stage pipes (e.g.
	// `ls | head -10 && git status`).
	program_word(first, src).map(str::to_owned)
}

fn classify_pipeline(pipeline: &ast::Pipeline, src: &str) -> Option<CommandPlan> {
	if pipeline.seq.len() > 1 {
		return Some(CommandPlan::Piped);
	}
	match pipeline.seq.first()? {
		ast::Command::Simple(simple) => {
			let program = program_word(simple, src)?;
			Some(CommandPlan::Single { program: program.to_owned() })
		},
		// Compound shell syntax (if / for / while / subshell / { ... }),
		// function definitions and `[[ … ]]` are not something the minimizer
		// should touch.
		ast::Command::Compound(..) | ast::Command::Function(_) | ast::Command::ExtendedTest(..) => {
			Some(CommandPlan::Compound)
		},
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	fn program_of(plan: CommandPlan) -> Option<String> {
		match plan {
			CommandPlan::Single { program } => Some(program),
			_ => None,
		}
	}

	fn chain_of(plan: CommandPlan) -> Option<Vec<ChainSegment>> {
		match plan {
			CommandPlan::Chain { segments } => Some(segments),
			_ => None,
		}
	}

	fn assert_not_chain(command: &str) {
		assert!(
			!matches!(analyze(command), CommandPlan::Chain { .. }),
			"{command:?} unexpectedly classified as Chain"
		);
	}

	#[test]
	fn single_simple_command() {
		let plan = analyze("git status --short");
		assert_eq!(program_of(plan), Some("git".to_string()));
	}

	#[test]
	fn env_prefix_is_still_single() {
		// env assignments are prefix, the program is `git`.
		let plan = analyze("FOO=1 git status");
		assert!(matches!(plan, CommandPlan::Single { .. }));
	}

	#[test]
	fn safe_and_chain_is_segmented() {
		let plan = analyze("git diff --stat && git diff --name-only");
		assert_eq!(
			chain_of(plan),
			Some(vec![
				ChainSegment {
					command:                   "git diff --stat".to_string(),
					source_at:                 0,
					source_len:                15,
					program:                   "git".to_string(),
					run_if_previous_succeeded: false,
					suppress_errexit:          true,
				},
				ChainSegment {
					command:                   "git diff --name-only".to_string(),
					source_at:                 19,
					source_len:                20,
					program:                   "git".to_string(),
					run_if_previous_succeeded: true,
					suppress_errexit:          false,
				},
			])
		);
	}

	#[test]
	fn safe_sequence_chain_is_segmented() {
		let plan = analyze("git status ; bun test");
		assert_eq!(
			chain_of(plan),
			Some(vec![
				ChainSegment {
					command:                   "git status".to_string(),
					source_at:                 0,
					source_len:                10,
					program:                   "git".to_string(),
					run_if_previous_succeeded: false,
					suppress_errexit:          false,
				},
				ChainSegment {
					command:                   "bun test".to_string(),
					source_at:                 13,
					source_len:                8,
					program:                   "bun".to_string(),
					run_if_previous_succeeded: false,
					suppress_errexit:          false,
				},
			])
		);
	}

	#[test]
	fn mixed_chain_is_segmented() {
		let plan = analyze("false && echo no ; echo yes");
		assert_eq!(
			chain_of(plan),
			Some(vec![
				ChainSegment {
					command:                   "false".to_string(),
					source_at:                 0,
					source_len:                5,
					program:                   "false".to_string(),
					run_if_previous_succeeded: false,
					suppress_errexit:          true,
				},
				ChainSegment {
					command:                   "echo no".to_string(),
					source_at:                 9,
					source_len:                7,
					program:                   "echo".to_string(),
					run_if_previous_succeeded: true,
					suppress_errexit:          false,
				},
				ChainSegment {
					command:                   "echo yes".to_string(),
					source_at:                 19,
					source_len:                8,
					program:                   "echo".to_string(),
					run_if_previous_succeeded: false,
					suppress_errexit:          false,
				},
			])
		);
	}

	#[test]
	fn chain_with_piped_segment_is_segmented() {
		// A chain that contains a piped segment (`ls | head -5`) must still be
		// classified as Chain so the segmented runner can decompose it. The
		// piped segment is identified by its first stage's program; the
		// per-segment minimizer::apply will treat that segment as Piped at
		// runtime and pass it through unchanged.
		let plan = analyze("ls -lh *.txt | head -5 && git status --short");
		let segments = chain_of(plan).expect("expected Chain");
		assert_eq!(segments.len(), 2);
		assert_eq!(segments[0].command, "ls -lh *.txt | head -5");
		assert_eq!(segments[0].program, "ls");
		assert_eq!(segments[1].program, "git");
	}

	#[test]
	fn segment_text_is_verbatim_source() {
		// Segments are sliced from the source, so quoting, spacing inside
		// quotes and redirect spelling survive exactly as written.
		let plan = analyze("echo 'a  b' \"c\"  >/dev/null 2>&1 && FOO=\"x y\" printf '%s\\n' z");
		let segments = chain_of(plan).expect("expected Chain");
		assert_eq!(segments[0].command, "echo 'a  b' \"c\"  >/dev/null 2>&1");
		assert_eq!(segments[0].program, "echo");
		assert_eq!(segments[1].command, "FOO=\"x y\" printf '%s\\n' z");
		assert_eq!(segments[1].program, "printf");
	}

	#[test]
	fn repeated_segments_keep_distinct_original_byte_spans() {
		let command = "  echo 'é' && echo 'é';\n echo 'é'";
		let segments = chain_of(analyze(command)).expect("expected Chain");
		assert_eq!(segments.len(), 3);
		assert_eq!(
			segments
				.iter()
				.map(|segment| (segment.source_at, segment.source_len))
				.collect::<Vec<_>>(),
			vec![(2, 9), (15, 9), (27, 9)],
		);
		for segment in segments {
			assert_eq!(segment.command, "echo 'é'");
			let start = segment.source_at as usize;
			let end = start + segment.source_len as usize;
			assert_eq!(&command[start..end], segment.command);
		}
	}

	#[test]
	fn rejects_unsafe_chain_segments() {
		for command in [
			"echo $(pwd) ; git status",
			"echo `pwd` ; git status",
			"echo \"$(pwd)\" ; git status",
			"echo ${x:-$(pwd)} ; git status",
			"echo ${ pwd;} ; git status",
			"cat <(printf hi) ; git status",
			"git status > >(cat) ; bun test",
			"! git status ; bun test",
		] {
			assert_not_chain(command);
		}
	}

	#[test]
	fn heredoc_chains_are_not_segmented() {
		// Regression: a chain whose segment carries a here-doc must not be
		// split. The document body lies outside the segment's source span, so
		// re-running the segment alone fails with "unterminated here
		// document". Both quoted and unquoted delimiters bail so the whole
		// command runs whole.
		assert_not_chain("cat <<'EOF'\nbody\nEOF\necho done");
		assert_not_chain("cat <<\"EOF\"\nbody\nEOF\necho done");
		assert_not_chain("cat <<EOF\nbody\nEOF\necho done");
		assert_not_chain("cat <<'EOF' && echo done\nbody\nEOF");
	}

	#[test]
	fn rejects_legacy_opaque_shapes() {
		assert_eq!(analyze("foo || bar"), CommandPlan::Compound);
		assert_eq!(analyze("git status | cat"), CommandPlan::Piped);
		assert_eq!(analyze("sleep 1 &"), CommandPlan::Compound);
		assert_eq!(analyze("(cd foo && make)"), CommandPlan::Compound);
		assert_eq!(analyze("{ echo hi; }"), CommandPlan::Compound);
		assert_eq!(analyze("f() { echo hi; }"), CommandPlan::Compound);
		assert_eq!(analyze("[[ -f foo ]]"), CommandPlan::Compound);
		assert_eq!(analyze("a && && b"), CommandPlan::Unsupported);
	}

	#[test]
	fn empty_is_unsupported() {
		assert_eq!(analyze(""), CommandPlan::Unsupported);
		assert_eq!(analyze("   "), CommandPlan::Unsupported);
	}

	#[test]
	fn compound_later_pipeline_stage_is_not_segmented() {
		// Regression: a pipeline whose *later* stage is a compound command
		// (`while`/`for`/`if`/subshell) must never be segmented. Validating only
		// the first stage (a simple `git`/`seq`) once let it slip through and
		// the re-run segment failed with "syntax error at end of input".
		assert_not_chain(
			"echo start && git log --oneline | while read h; do echo \"$h\"; done | head -3",
		);
		assert_not_chain("seq 3 | while read n; do echo \"$n\"; done && echo done");
		assert_not_chain("printf x && ls | for f in a b; do echo $f; done");
		// The reported shape classifies as Compound, so it runs whole and
		// unsegmented via the single path (no minimization).
		assert_eq!(
			analyze("echo a && seq 2 | while read n; do echo $n; done"),
			CommandPlan::Compound,
		);
	}

	#[test]
	fn chain_segments_reparse_cleanly() {
		// Every command string the chain runner will execute must itself parse
		// to exactly one pipeline with no `&&`/`||`/`;`/`&` continuation.
		for command in [
			"git diff --stat && git diff --name-only",
			"ls -lh *.txt | head -5 && git status --short",
			"grep -c foo bar 2>/dev/null && echo done",
			"FOO=1 git status ; bun test",
		] {
			let Some(segments) = chain_of(analyze(command)) else {
				panic!("{command:?} should classify as Chain");
			};
			for segment in &segments {
				let (_, program) = parse(&segment.command).unwrap_or_else(|| {
					panic!("segment {:?} from {command:?} did not re-parse", segment.command)
				});
				let items: Vec<&ast::CompoundListItem> = program
					.complete_commands
					.iter()
					.flat_map(|complete| complete.0.iter())
					.collect();
				let single_pipeline = matches!(
					items.as_slice(),
					[item] if item.0.additional.is_empty() && !is_async(&item.1)
				);
				assert!(
					single_pipeline,
					"segment {:?} from {command:?} re-parsed to a different shape",
					segment.command,
				);
			}
		}
	}
}

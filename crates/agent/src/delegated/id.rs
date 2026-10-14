//! Branded string identifiers of the delegated-executor contract.

omp_core::string_id!(
	/// Stable identity of one executor backend, for example `codex-app-server`.
	ExecutorId
);

omp_core::string_id!(
	/// Session identity assigned by the delegated CLI, never by OMP. It is only
	/// ever echoed back to the same CLI to resume.
	CliSessionId
);

omp_core::string_id!(
	/// Turn identity assigned by the delegated CLI.
	TurnId
);

omp_core::string_id!(
	/// Identity of one command or tool call inside a delegated turn.
	CallId
);

omp_core::string_id!(
	/// Identity of one approval prompt raised by the delegated CLI.
	PromptId
);

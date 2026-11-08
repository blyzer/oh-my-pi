#!/usr/bin/env bash
set -uo pipefail
tool=$1
out=$2/$tool
work=$(mktemp -d)
mkdir -p "$out"

record() {
	local case=$1
	shift
	(cd "$work/$case" && "$@" >"$out/$case.stdout" 2>"$out/$case.stderr")
	echo $? >"$out/$case.exit"
}

project() {
	mkdir -p "$work/fail" "$work/clean"
	printf '%s' "$2" >"$work/fail/$1"
	printf '%s' "$3" >"$work/clean/$1"
}

case $tool in
rubocop)
	rubocop --version >"$out/version"
	project a.rb $'def f( x )\n  y=1\n  return x\nend\n' $'# frozen_string_literal: true\n\n# Doc.\nmodule A\n  def self.f(value)\n    value\n  end\nend\n'
	for c in fail clean; do record $c rubocop --format json; done
	;;
phpstan)
	phpstan --version >"$out/version"
	project a.php $'<?php\nfunction f(): int { return "x"; }\nundefined_fn();\n' $'<?php\nfunction f(): int { return 1; }\n'
	for c in fail clean; do printf 'parameters:\n  level: 9\n  paths: [.]\n' >"$work/$c/phpstan.neon"; record $c phpstan analyse --error-format=json --no-progress; done
	;;
psalm)
	psalm --version >"$out/version"
	project a.php $'<?php\nfunction f(): int { return "x"; }\nundefined_fn();\n' $'<?php\nfunction f(): int { return 1; }\n'
	for c in fail clean; do printf '<?xml version="1.0"?>\n<psalm errorLevel="1"><projectFiles><directory name="."/></projectFiles></psalm>\n' >"$work/$c/psalm.xml"; record $c psalm --output-format=json --no-cache; done
	;;
hlint)
	hlint --version >"$out/version"
	project A.hs $'module A where\nf x = map (\\y -> y) (concat (map g x))\n  where g = id\n' $'module A where\n\nf :: Int -> Int\nf x = x + 1\n'
	for c in fail clean; do record $c hlint . --json; done
	;;
tflint)
	tflint --version >"$out/version"
	project main.tf $'terraform {\n  required_version = ">= 1.0"\n}\nvariable "unused" {}\nlocals {\n  x = "${var.unused}"\n}\n' $'terraform {\n  required_version = ">= 1.0"\n}\n'
	touch "$work/clean/outputs.tf" "$work/clean/variables.tf"
	for c in fail clean; do printf 'plugin "terraform" {\n  enabled = true\n  preset  = "all"\n}\n' >"$work/$c/.tflint.hcl"; (cd "$work/$c" && tflint --init >/dev/null 2>&1); record $c tflint --format=json; done
	;;
credo)
	for c in fail clean; do
		(cd "$work" && mix new "$c" >/dev/null)
		sed -i.bak 's/deps do/deps do\n    [{:credo, "~> 1.7", only: [:dev, :test], runtime: false}] ++/' "$work/$c/mix.exs"
		(cd "$work/$c" && mix deps.get >/dev/null 2>&1)
	done
	printf '%s' $'defmodule Fail do\n  def f(x) do\n    IO.inspect(x)\n    if !x, do: 1, else: 2\n  end\nend\n' >"$work/fail/lib/fail.ex"
	(cd "$work/clean" && mix credo --version >"$out/version" 2>/dev/null)
	record fail mix credo --format json
	(cd "$work/clean" && mix compile >/dev/null 2>&1)
	rm -rf "$work/clean/_build"
	record clean mix credo --format json
	;;
swiftlint)
	swiftlint version >"$out/version"
	project a.swift $'let x : Int = 1;\nfunc f() { var y = 1 }\n' $'let value = 1\n'
	for c in fail clean; do record $c swiftlint lint --reporter json; done
	;;
*)
	echo "unknown tool $tool" >&2
	exit 2
	;;
esac
echo "$tool: fail exit $(cat "$out/fail.exit"), clean exit $(cat "$out/clean.exit")"
[ "$(cat "$out/fail.exit")" != 0 ] || { echo "$tool: failing input passed" >&2; exit 1; }
[ "$(cat "$out/clean.exit")" = 0 ] || { echo "$tool: clean input failed" >&2; exit 1; }

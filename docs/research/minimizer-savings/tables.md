Tokens are `o200k` unless a table names another counter.

## Per capture

| Capture | Command | Exit | Source | Band | Filter | Raw bytes | Rewrite bytes | Raw tok | Today tok | Minimized tok | Saved | Failure lines (of / today / minimized) |
|---|---|--:|---|---|---|--:|--:|--:|--:|--:|--:|---|
| gha-127410375533-cargo-fmt-all-check | `cargo fmt --all -- --check` | 0 | gha-job:127410375533 | under 1,000 chars | too-short (unchanged) | 0 | 0 | 0 | 0 | 0 | 0.0% | — |
| git-status-short-checkout | `git status --short` | 0 | local-macos | under 1,000 chars | too-short (unchanged) | 37 | 37 | 12 | 12 | 12 | 0.0% | — |
| gha-127410375505-python3-scripts-check-python-examples-py | `python3 scripts/check-python-examples.py` | 0 | gha-job:127410375505 | under 1,000 chars | unsupported (unchanged) | 43 | 43 | 13 | 13 | 13 | 0.0% | — |
| gha-127411153221-cargo-run-quiet-release-locked-manifest-path-tools-lintx-car | `cargo run --quiet --release --locked --manifest-path tools/lintx/Cargo.toml -- --ratchet tools/lintx/baselines/error-formatting.toml crates` | 0 | gha-job:127411153221 | under 1,000 chars | too-short (unchanged) | 44 | 44 | 14 | 14 | 14 | 0.0% | — |
| gh-issue-list | `gh issue list` | 1 | local-macos | under 1,000 chars | too-short (unchanged) | 53 | 53 | 15 | 15 | 15 | 0.0% | 0 / 0 / 0 |
| py-check-ci-paths | `python3 scripts/check-ci-paths.py` | 0 | local-macos | under 1,000 chars | unsupported (unchanged) | 55 | 55 | 16 | 16 | 16 | 0.0% | — |
| py-check-editing-discipline | `python3 scripts/check-editing-discipline.py` | 0 | local-macos | under 1,000 chars | unsupported (unchanged) | 57 | 57 | 11 | 11 | 11 | 0.0% | — |
| git-status-clean | `git status` | 0 | local-macos | under 1,000 chars | too-short (unchanged) | 71 | 71 | 17 | 17 | 17 | 0.0% | — |
| git-diff-stat-head1 | `git diff HEAD~1 --stat` | 0 | local-macos | under 1,000 chars | too-short (unchanged) | 94 | 94 | 30 | 30 | 30 | 0.0% | — |
| git-fetch-origin | `git fetch origin` | 0 | local-macos | under 1,000 chars | too-short (unchanged) | 141 | 141 | 40 | 40 | 40 | 0.0% | — |
| gha-127410375505-cargo-nextest-show-config-version | `cargo nextest show-config version` | 0 | gha-job:127410375505 | under 1,000 chars | too-short (unchanged) | 187 | 187 | 50 | 50 | 50 | 0.0% | — |
| git-remote-v | `git remote -v` | 0 | local-macos | under 1,000 chars | unsupported (unchanged) | 188 | 188 | 66 | 66 | 66 | 0.0% | — |
| just-tools-test | `just tools-test` | 0 | local-macos | under 1,000 chars | too-short (unchanged) | 250 | 250 | 97 | 97 | 97 | 0.0% | — |
| git-worktree-list | `git worktree list` | 0 | local-macos | under 1,000 chars | too-short (unchanged) | 307 | 307 | 94 | 94 | 94 | 0.0% | — |
| git-status-checkout | `git status` | 0 | local-macos | under 1,000 chars | too-short (unchanged) | 334 | 334 | 92 | 92 | 92 | 0.0% | — |
| ls-crates | `ls crates` | 0 | local-macos | under 1,000 chars | too-short (unchanged) | 357 | 357 | 135 | 135 | 135 | 0.0% | — |
| git-show-stat-head | `git show --stat HEAD` | 0 | local-macos | under 1,000 chars | too-short (unchanged) | 369 | 369 | 144 | 144 | 144 | 0.0% | — |
| uv-run-pytest-scripts | `uv run --with pytest pytest scripts/test_quiet_host.py scripts/test_measure_adr0040.py` | 0 | local-macos | under 1,000 chars | too-short (unchanged) | 449 | 449 | 100 | 100 | 100 | 0.0% | — |
| gha-127264733848-cargo-fmt-all-check | `cargo fmt --all -- --check` | 1 | gha-job:127264733848 | under 1,000 chars | too-short (unchanged) | 463 | 463 | 130 | 130 | 130 | 0.0% | 1 / 1 / 1 |
| rg-fn-apply-minimizer | `rg -n 'fn apply' crates/minimizer` | 0 | local-macos | under 1,000 chars | too-short (unchanged) | 502 | 502 | 165 | 165 | 165 | 0.0% | — |
| gh-pr-view-515-comments | `gh pr view 515 --comments` | 0 | local-macos | under 1,000 chars | too-short (unchanged) | 593 | 593 | 147 | 147 | 147 | 0.0% | — |
| grep-rn-default-max-output | `grep -rn 'DEFAULT_MAX_OUTPUT_BYTES' crates` | 0 | local-macos | under 1,000 chars | too-short (unchanged) | 773 | 773 | 204 | 204 | 204 | 0.0% | — |
| find-minimizer-rs | `find crates/minimizer -name '*.rs'` | 0 | local-macos | under 1,000 chars | too-short (unchanged) | 781 | 781 | 242 | 242 | 242 | 0.0% | — |
| cat-minimizer-cargo-toml | `cat crates/minimizer/Cargo.toml` | 0 | local-macos | under 1,000 chars | too-short (unchanged) | 809 | 809 | 192 | 192 | 192 | 0.0% | — |
| wc-l-minimizer | `wc -l crates/minimizer/src/*.rs crates/minimizer/src/filters/*.rs` | 0 | local-macos | under 1,000 chars | too-short (unchanged) | 905 | 905 | 274 | 274 | 274 | 0.0% | — |
| du-sh-crates | `du -sh crates/*` | 0 | local-macos | 1,000 chars to 16 KiB | du | 1017 | 455 | 418 | 418 | 244 | 41.6% | — |
| ls-la-filters | `ls -la crates/minimizer/src/filters` | 0 | local-macos | 1,000 chars to 16 KiB | ls (unchanged) | 1067 | 1067 | 507 | 507 | 507 | 0.0% | — |
| git-log-oneline-pr515 | `git log --oneline 69895da5a499a7e239058a3cd5bea89e32e185db..df8b485bcf646cf535af4dbaec0e64f7d6a9008c` | 0 | local-macos | 1,000 chars to 16 KiB | git (unchanged) | 1268 | 1268 | 403 | 403 | 403 | 0.0% | — |
| gh-pr-list | `gh pr list` | 0 | local-macos | 1,000 chars to 16 KiB | gh (unchanged) | 1308 | 1308 | 440 | 440 | 440 | 0.0% | — |
| find-crates-cargo-toml | `find crates -name Cargo.toml` | 0 | local-macos | 1,000 chars to 16 KiB | find | 1347 | 454 | 491 | 491 | 200 | 59.3% | — |
| git-log-oneline-20 | `git log --oneline -20` | 0 | local-macos | 1,000 chars to 16 KiB | git (unchanged) | 1515 | 1515 | 475 | 475 | 475 | 0.0% | — |
| gha-127410375481-just-license-check | `just license-check` | 0 | gha-job:127410375481 | 1,000 chars to 16 KiB | pipeline | 1645 | 1643 | 560 | 560 | 611 | -9.1% | — |
| gh-run-view-42074202980 | `gh run view 42074202980` | 0 | local-macos | 1,000 chars to 16 KiB | gh | 1705 | 1710 | 522 | 522 | 579 | -10.9% | — |
| gh-pr-checks-515 | `gh pr checks 515` | 0 | local-macos | 1,000 chars to 16 KiB | gh | 1957 | 1940 | 645 | 645 | 692 | -7.3% | — |
| gha-127411153221-cargo-test-locked-manifest-path-tools-lintx-cargo-toml | `cargo test --locked --manifest-path tools/lintx/Cargo.toml` | 0 | gha-job:127411153221 | 1,000 chars to 16 KiB | cargo | 1976 | 39 | 549 | 549 | 70 | 87.2% | — |
| bb-cargo-test-doc-minimizer | `cargo test --doc -p omp-minimizer --locked` | 0 | buildbuddy-linux:just-remote-test-pkg-minimizer | 1,000 chars to 16 KiB | cargo | 2025 | 44 | 565 | 565 | 69 | 87.8% | — |
| grep-rn-unwrap-agent | `grep -rn 'unwrap()' crates/agent/src` | 0 | local-macos | 1,000 chars to 16 KiB | grep | 2093 | 679 | 612 | 612 | 249 | 59.3% | — |
| ls-la-root | `ls -la` | 0 | local-macos | 1,000 chars to 16 KiB | ls | 2532 | 549 | 1149 | 1149 | 298 | 74.1% | — |
| git-diff-name-only-pr515 | `git diff --name-only 69895da5a499a7e239058a3cd5bea89e32e185db..df8b485bcf646cf535af4dbaec0e64f7d6a9008c` | 0 | local-macos | 1,000 chars to 16 KiB | git | 2736 | 810 | 936 | 936 | 334 | 64.3% | — |
| uv-run-pytest-scripts-v | `uv run --with pytest pytest -v scripts/test_quiet_host.py scripts/test_measure_adr0040.py` | 0 | local-macos | 1,000 chars to 16 KiB | uv (unchanged) | 2831 | 2831 | 764 | 764 | 764 | 0.0% | — |
| git-show-ba975d2808 | `git show ba975d2808` | 0 | local-macos | 1,000 chars to 16 KiB | git (unchanged) | 2972 | 2972 | 952 | 952 | 952 | 0.0% | — |
| gh-run-list-20 | `gh run list --limit 20` | 0 | local-macos | 1,000 chars to 16 KiB | gh (unchanged) | 3467 | 3467 | 1161 | 1161 | 1161 | 0.0% | — |
| gh-pr-list-merged-30 | `gh pr list --state merged --limit 30` | 0 | local-macos | 1,000 chars to 16 KiB | gh (unchanged) | 3544 | 3544 | 1231 | 1231 | 1231 | 0.0% | — |
| tree-minimizer | `tree crates/minimizer` | 0 | local-macos | 1,000 chars to 16 KiB | tree | 3794 | 2790 | 1124 | 1124 | 881 | 21.6% | — |
| git-diff-stat-pr515 | `git diff 69895da5a499a7e239058a3cd5bea89e32e185db..df8b485bcf646cf535af4dbaec0e64f7d6a9008c --stat` | 0 | local-macos | 1,000 chars to 16 KiB | git | 4183 | 1200 | 1250 | 1250 | 437 | 65.0% | — |
| git-show-13afaa6917 | `git show 13afaa6917` | 0 | local-macos | 1,000 chars to 16 KiB | git (unchanged) | 4707 | 4707 | 1573 | 1573 | 1573 | 0.0% | — |
| git-branch-r | `git branch -r` | 0 | local-macos | 1,000 chars to 16 KiB | git | 6793 | 804 | 2154 | 2154 | 272 | 87.4% | — |
| git-log-stat-5 | `git log --stat -5` | 0 | local-macos | 1,000 chars to 16 KiB | git | 7948 | 1036 | 2391 | 2391 | 360 | 84.9% | — |
| bb-cargo-check-minimizer | `cargo check -p omp-minimizer --locked` | 0 | buildbuddy-linux:just-remote-check-pkg-minimizer | 1,000 chars to 16 KiB | cargo | 7990 | 1726 | 2770 | 2770 | 518 | 81.3% | — |
| bb-just-check-pkg-minimizer | `just check-pkg omp-minimizer` | 0 | buildbuddy-linux:just-remote-check-pkg-minimizer | 1,000 chars to 16 KiB | pipeline | 8028 | 2512 | 2781 | 2781 | 959 | 65.5% | — |
| rg-minimizeroutput | `rg -n 'MinimizerOutput' crates` | 0 | local-macos | 1,000 chars to 16 KiB | rg | 8957 | 3234 | 2780 | 2780 | 1027 | 63.1% | — |
| git-log-10 | `git log -10` | 0 | local-macos | 1,000 chars to 16 KiB | git | 10002 | 2134 | 2968 | 2968 | 688 | 76.8% | — |
| gh-pr-view-515 | `gh pr view 515` | 0 | local-macos | 1,000 chars to 16 KiB | gh | 10963 | 10959 | 4338 | 4338 | 4387 | -1.1% | — |
| git-diff-engine-pr515 | `git diff 69895da5a499a7e239058a3cd5bea89e32e185db..df8b485bcf646cf535af4dbaec0e64f7d6a9008c -- crates/minimizer/src/engine.rs` | 0 | local-macos | 1,000 chars to 16 KiB | git (unchanged) | 12850 | 12850 | 4010 | 4010 | 4010 | 0.0% | — |
| gha-127410375741-scripts-remote-verify-sh-remote-probe | `scripts/remote-verify.sh remote-probe` | 0 | gha-job:127410375741 | 16 KiB to 50 KiB | unsupported (unchanged) | 16665 | 16665 | 4871 | 4871 | 4871 | 0.0% | — |
| cat-justfile | `cat justfile` | 0 | local-macos | 16 KiB to 50 KiB | cat (unchanged) | 17254 | 17254 | 4223 | 4223 | 4223 | 0.0% | — |
| gh-run-view-job-127410375533-log | `gh run view --job 127410375533 --log` | 0 | local-macos | 16 KiB to 50 KiB | gh (unchanged) | 17828 | 17828 | 6598 | 6598 | 6598 | 0.0% | — |
| gh-api-pulls-515 | `gh api repos/blyzer/oh-my-pi/pulls/515` | 0 | local-macos | 16 KiB to 50 KiB | gh (unchanged) | 26360 | 26360 | 9010 | 242 | 242 | 0.0% | — |
| just-remote-check-pkg-minimizer | `just remote check-pkg omp-minimizer` | 0 | local-macos | 16 KiB to 50 KiB | pipeline | 27252 | 4655 | 10204 | 10204 | 1433 | 86.0% | — |
| gha-127410375505-scripts-ssh-loopback-fixture-sh-cargo-nextest-run-profile-ci | `scripts/ssh-loopback-fixture.sh cargo nextest run --profile ci -p omp-exec-ssh --locked --run-ignored all` | 0 | gha-job:127410375505 | 16 KiB to 50 KiB | unsupported (unchanged) | 31461 | 31461 | 8249 | 8249 | 8249 | 0.0% | — |
| gha-124490258584-cargo-nextest-run-profile-ci-workspace-no-run-locked | `cargo nextest run --profile ci --workspace --no-run --locked` | 101 | gha-job:124490258584 | 16 KiB to 50 KiB | cargo | 46001 | 6080 | 12079 | 12079 | 1588 | 86.9% | 12 / 12 / 7 |
| bb-cargo-nextest-minimizer | `cargo nextest run -p omp-minimizer --locked` | 0 | buildbuddy-linux:just-remote-test-pkg-minimizer | 16 KiB to 50 KiB | cargo | 47841 | 57 | 13959 | 13959 | 73 | 99.5% | — |
| cat-agents-md | `cat AGENTS.md` | 0 | local-macos | 16 KiB to 50 KiB | cat (unchanged) | 48285 | 48285 | 12721 | 12721 | 12721 | 0.0% | — |
| cat-engine-rs | `cat crates/minimizer/src/engine.rs` | 0 | local-macos | 16 KiB to 50 KiB | cat | 49263 | 1928 | 13753 | 13753 | 566 | 95.9% | — |
| bb-just-test-pkg-minimizer | `just test-pkg omp-minimizer` | 0 | buildbuddy-linux:just-remote-test-pkg-minimizer | 16 KiB to 50 KiB | pipeline | 49953 | 5690 | 14550 | 14550 | 1686 | 88.4% | — |
| gha-127410375505-cargo-nextest-run-profile-ci-p-omp-e2e-test-p12-daemon-appro | `cargo nextest run --profile ci -p omp-e2e --test p12_daemon_approvals --locked` | 0 | gha-job:127410375505 | 16 KiB to 50 KiB | cargo | 50983 | 53 | 13600 | 13600 | 80 | 99.4% | — |
| gha-127410375505-cargo-nextest-run-profile-ci-p-omp-e2e-test-p8-baselines-loc | `cargo nextest run --profile ci -p omp-e2e --test p8_baselines --locked` | 0 | gha-job:127410375505 | 50 KiB to 64 KiB | cargo | 51680 | 55 | 13826 | 9091 | 71 | 99.2% | — |
| gha-127410375505-cargo-nextest-run-profile-ci-p-omp-e2e-test-p7-tui-test-p7-t | `cargo nextest run --profile ci -p omp-e2e --test p7_tui --test p7_tsp --locked` | 0 | gha-job:127410375505 | 50 KiB to 64 KiB | cargo | 52717 | 55 | 14244 | 9434 | 78 | 99.2% | — |
| gha-127411153344-cargo-nextest-run-profile-ci-p-omp-e2e-test-tool-sources-loc | `cargo nextest run --profile ci -p omp-e2e --test tool_sources --locked` | 0 | gha-job:127411153344 | 50 KiB to 64 KiB | cargo | 53903 | 53 | 14208 | 8902 | 76 | 99.1% | — |
| gha-127411153456-cargo-nextest-run-profile-ci-p-omp-e2e-test-p7-tui-test-p7-t | `cargo nextest run --profile ci -p omp-e2e --test p7_tui --test p7_tsp --locked` | 0 | gha-job:127411153456 | 50 KiB to 64 KiB | cargo | 55453 | 55 | 14808 | 9389 | 71 | 99.2% | — |
| gha-126488879099-cargo-nextest-run-profile-ci-p-omp-e2e-test-p7-tui-test-p7-t | `cargo nextest run --profile ci -p omp-e2e --test p7_tui --test p7_tsp --locked` | 100 | gha-job:126488879099 | 50 KiB to 64 KiB | cargo | 55656 | 1368 | 15096 | 9757 | 465 | 95.2% | 4 / 4 / 3 |
| gha-127410375505-cargo-test-doc-workspace-exclude-omp-e2e-locked | `cargo test --doc --workspace --exclude omp-e2e --locked` | 0 | gha-job:127410375505 | over 64 KiB | cargo | 77145 | 68 | 21519 | 10753 | 73 | 99.3% | — |
| just-remote-test-pkg-minimizer | `just remote test-pkg omp-minimizer` | 0 | local-macos | over 64 KiB | pipeline | 80816 | 4772 | 33280 | 15623 | 1453 | 90.7% | — |
| gha-127410375505-cargo-nextest-run-profile-ci-workspace-no-run-locked | `cargo nextest run --profile ci --workspace --no-run --locked` | 0 | gha-job:127410375505 | over 64 KiB | cargo | 96356 | 6010 | 25907 | 10736 | 1600 | 85.1% | — |
| gha-126882651164-scripts-remote-verify-sh-remote-heavy | `scripts/remote-verify.sh remote-heavy` | 101 | gha-job:126882651164 | over 64 KiB | unsupported (unchanged) | 99961 | 99961 | 26722 | 10575 | 10575 | 0.0% | 4 / 0 / 0 |
| rg-todo-crates | `rg -n 'TODO' crates` | 0 | local-macos | over 64 KiB | rg | 124697 | 2713 | 29897 | 6476 | 794 | 87.7% | — |
| cat-dispatch-rs | `cat crates/agent/src/dispatch.rs` | 0 | local-macos | over 64 KiB | cat | 144249 | 3503 | 37699 | 8545 | 917 | 89.3% | — |
| git-diff-pr515 | `git diff 69895da5a499a7e239058a3cd5bea89e32e185db..df8b485bcf646cf535af4dbaec0e64f7d6a9008c` | 0 | local-macos | over 64 KiB | git | 335860 | 7278 | 103195 | 10877 | 2225 | 79.5% | — |
| gh-pr-diff-515 | `gh pr diff 515` | 0 | local-macos | over 64 KiB | gh (unchanged) | 336140 | 336140 | 103273 | 10917 | 10917 | 0.0% | — |
| gh-run-view-41944709223-log-failed | `gh run view 41944709223 --log-failed` | 0 | local-macos | over 64 KiB | gh (unchanged) | 586904 | 586904 | 202364 | 13977 | 13977 | 0.0% | — |
| gha-126973989391-scripts-remote-verify-sh-remote-heavy | `scripts/remote-verify.sh remote-heavy` | 101 | gha-job:126973989391 | over 64 KiB | unsupported (unchanged) | 685224 | 685224 | 214176 | 11239 | 11239 | 0.0% | 8 / 0 / 0 |
| gha-127207059094-cargo-nextest-run-profile-ci-workspace-exclude-omp-e2e-locke | `cargo nextest run --profile ci --workspace --exclude omp-e2e --locked` | 100 | gha-job:127207059094 | over 64 KiB | cargo | 1379649 | 84 | 429531 | 10658 | 86 | 99.2% | 2 / 0 / 0 |
| gha-127218465319-cargo-nextest-run-profile-ci-workspace-exclude-omp-e2e-locke | `cargo nextest run --profile ci --workspace --exclude omp-e2e --locked` | 100 | gha-job:127218465319 | over 64 KiB | cargo | 1382191 | 129 | 430366 | 10665 | 90 | 99.2% | 2 / 0 / 0 |
| gha-127264733989-cargo-nextest-run-profile-ci-workspace-exclude-omp-e2e-locke | `cargo nextest run --profile ci --workspace --exclude omp-e2e --locked` | 100 | gha-job:127264733989 | over 64 KiB | cargo | 1383992 | 132 | 430831 | 10683 | 91 | 99.1% | 2 / 0 / 0 |
| gha-127410375505-cargo-nextest-run-profile-ci-workspace-exclude-omp-e2e-locke | `cargo nextest run --profile ci --workspace --exclude omp-e2e --locked` | 0 | gha-job:127410375505 | over 64 KiB | cargo | 1419468 | 71 | 442281 | 10667 | 79 | 99.3% | — |
| gha-124490258597-cargo-clippy-workspace-all-targets-locked | `cargo clippy --workspace --all-targets --locked` | 101 | gha-job:124490258597 | over 64 KiB | cargo | 1445872 | 8896 | 364504 | 10191 | 2231 | 78.1% | 11 / 0 / 11 |
| gha-127411153301-cargo-clippy-workspace-all-targets-locked-keep-going | `cargo clippy --workspace --all-targets --locked --keep-going` | 0 | gha-job:127411153301 | over 64 KiB | cargo | 1983296 | 356174 | 498158 | 10280 | 11240 | -9.3% | — |
| gha-127410375331-cargo-clippy-workspace-all-targets-locked | `cargo clippy --workspace --all-targets --locked` | 0 | gha-job:127410375331 | over 64 KiB | cargo | 1993243 | 351322 | 504459 | 11840 | 11280 | 4.7% | — |
| gha-127410375741-scripts-remote-verify-sh-remote-heavy | `scripts/remote-verify.sh remote-heavy` | 0 | gha-job:127410375741 | over 64 KiB | unsupported (unchanged) | 3340436 | 3340436 | 876625 | 11052 | 11052 | 0.0% | — |
| gh-run-view-42019489927-log-failed | `gh run view 42019489927 --log-failed` | 0 | local-macos | over 64 KiB | gh (unchanged) | 3657426 | 3657426 | 1495189 | 14501 | 14501 | 0.0% | — |
| gh-run-view-42033217186-log-failed | `gh run view 42033217186 --log-failed` | 0 | local-macos | over 64 KiB | gh (unchanged) | 3666366 | 3666366 | 1498510 | 14842 | 14842 | 0.0% | — |
| gh-run-view-41804508029-log-failed | `gh run view 41804508029 --log-failed` | 0 | local-macos | over 64 KiB | too-large (unchanged) | 4317783 | 4317783 | 1741512 | 14447 | 14447 | 0.0% | — |

## By size band

| Band | Captures | Rewritten | Raw tok | Today tok | Minimized tok | Saved | Per-capture saved: min / median / max |
|---|--:|--:|--:|--:|--:|--:|---|
| 1,000 chars to 16 KiB | 29 | 19 | 40519 | 40519 | 24391 | 39.8% | -10.9% / 21.6% / 87.8% |
| 16 KiB to 50 KiB | 12 | 6 | 123817 | 115049 | 42330 | 63.2% | 0.0% / 0.0% / 99.5% |
| 50 KiB to 64 KiB | 5 | 5 | 72182 | 46573 | 761 | 98.4% | 95.2% / 99.2% / 99.2% |
| over 64 KiB | 21 | 13 | 9509998 | 239544 | 133709 | 44.2% | -9.3% / 78.1% / 99.3% |
| under 1,000 chars | 25 | 0 | 2300 | 2300 | 2300 | 0.0% | 0.0% / 0.0% / 0.0% |

## By program

| Program | Captures | Rewritten | Raw tok | Today tok | Minimized tok | Saved | Per-capture saved: min / median / max |
|---|--:|--:|--:|--:|--:|--:|---|
| cargo | 24 | 20 | 3263454 | 186762 | 30123 | 83.9% | -9.3% / 87.8% / 99.5% |
| cat | 5 | 2 | 68588 | 39434 | 18619 | 52.8% | 0.0% / 0.0% / 95.9% |
| du | 1 | 1 | 418 | 418 | 244 | 41.6% | 41.6% / 41.6% / 41.6% |
| find | 2 | 1 | 733 | 733 | 442 | 39.7% | 0.0% / 0.0% / 59.3% |
| gh | 15 | 3 | 5064955 | 84023 | 84176 | -0.2% | -10.9% / 0.0% / 0.0% |
| git | 19 | 6 | 120802 | 28484 | 12224 | 57.1% | 0.0% / 0.0% / 87.4% |
| grep | 2 | 1 | 816 | 816 | 453 | 44.5% | 0.0% / 0.0% / 59.3% |
| just | 6 | 5 | 61472 | 43815 | 6239 | 85.8% | -9.1% / 65.5% / 90.7% |
| ls | 3 | 1 | 1791 | 1791 | 940 | 47.5% | 0.0% / 0.0% / 74.1% |
| python3 | 3 | 0 | 40 | 40 | 40 | 0.0% | 0.0% / 0.0% / 0.0% |
| remote-verify.sh | 4 | 0 | 1122394 | 37737 | 37737 | 0.0% | 0.0% / 0.0% / 0.0% |
| rg | 3 | 2 | 32842 | 9421 | 1986 | 78.9% | 0.0% / 63.1% / 87.7% |
| ssh-loopback-fixture.sh | 1 | 0 | 8249 | 8249 | 8249 | 0.0% | 0.0% / 0.0% / 0.0% |
| tree | 1 | 1 | 1124 | 1124 | 881 | 21.6% | 21.6% / 21.6% / 21.6% |
| uv | 2 | 0 | 864 | 864 | 864 | 0.0% | 0.0% / 0.0% / 0.0% |
| wc | 1 | 0 | 274 | 274 | 274 | 0.0% | 0.0% / 0.0% / 0.0% |

## By counter

| Counter | Today tok, all | Minimized tok, all | Saved, all | Today tok, ≤ 50 KiB | Minimized tok, ≤ 50 KiB | Saved, ≤ 50 KiB |
|---|--:|--:|--:|--:|--:|--:|
| o200k | 443985 | 203491 | 54.2% | 157868 | 69021 | 56.3% |
| claude-legacy | 494559 | 225630 | 54.4% | 176287 | 76837 | 56.4% |
| bytes/4 | 380568 | 168828 | 55.6% | 133688 | 57917 | 56.7% |

## Failure lines in failing captures

| Capture | Command | Exit | Filter | Failure lines | Visible today | Visible minimized |
|---|---|--:|---|--:|--:|--:|
| gh-issue-list | `gh issue list` | 1 | too-short | 0 | 0 | 0 |
| gha-127264733848-cargo-fmt-all-check | `cargo fmt --all -- --check` | 1 | too-short | 1 | 1 | 1 |
| gha-124490258584-cargo-nextest-run-profile-ci-workspace-no-run-locked | `cargo nextest run --profile ci --workspace --no-run --locked` | 101 | cargo | 12 | 12 | 7 |
| gha-126488879099-cargo-nextest-run-profile-ci-p-omp-e2e-test-p7-tui-test-p7-t | `cargo nextest run --profile ci -p omp-e2e --test p7_tui --test p7_tsp --locked` | 100 | cargo | 4 | 4 | 3 |
| gha-126882651164-scripts-remote-verify-sh-remote-heavy | `scripts/remote-verify.sh remote-heavy` | 101 | unsupported | 4 | 0 | 0 |
| gha-126973989391-scripts-remote-verify-sh-remote-heavy | `scripts/remote-verify.sh remote-heavy` | 101 | unsupported | 8 | 0 | 0 |
| gha-127207059094-cargo-nextest-run-profile-ci-workspace-exclude-omp-e2e-locke | `cargo nextest run --profile ci --workspace --exclude omp-e2e --locked` | 100 | cargo | 2 | 0 | 0 |
| gha-127218465319-cargo-nextest-run-profile-ci-workspace-exclude-omp-e2e-locke | `cargo nextest run --profile ci --workspace --exclude omp-e2e --locked` | 100 | cargo | 2 | 0 | 0 |
| gha-127264733989-cargo-nextest-run-profile-ci-workspace-exclude-omp-e2e-locke | `cargo nextest run --profile ci --workspace --exclude omp-e2e --locked` | 100 | cargo | 2 | 0 | 0 |
| gha-124490258597-cargo-clippy-workspace-all-targets-locked | `cargo clippy --workspace --all-targets --locked` | 101 | cargo | 11 | 0 | 11 |

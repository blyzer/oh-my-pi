#!/usr/bin/env bash
# check-pkg-drift.sh — drift guard for exec-image/Dockerfile vs remote-verify-runner.sh.
#
# The BuildBuddy runner (remote-verify-runner.sh) installs packages at boot to
# supplement what the image already carries.  Any package the runner installs
# that is absent from the Dockerfile means the image is missing that tool and
# cold runs (no recycled runner state) will fail.
#
# Rule: every package in the runner's `apt-get install -y` line must also
# appear in the Dockerfile's `apt-get install` RUN step.
#
# The Dockerfile may carry extras (e.g. ca-certificates, pkg-config) that the
# runner skips because the base Ubuntu image already has them.
#
# Exit 0 when the runner packages are a subset of the Dockerfile packages.
# Exit 1 and list the missing packages otherwise.
set -euo pipefail

SCRIPT_DIR=$(cd "$(dirname "$0")" && pwd)
REPO_ROOT=$(cd "$SCRIPT_DIR/../.." && pwd)
DOCKERFILE="$SCRIPT_DIR/Dockerfile"
RUNNER="$REPO_ROOT/scripts/remote-verify-runner.sh"

# Extract package names from the Dockerfile's `apt-get install` RUN step.
dockerfile_pkgs() {
    awk '
        /apt-get install/ { collecting=1 }
        collecting {
            gsub(/\\$/, ""); gsub(/^[[:space:]]+/, "")
            if (/apt-get install/) {
                sub(/.*--no-install-recommends[[:space:]]*/, "")
                sub(/.*install -y[[:space:]]*/, "")
            }
            if (/&&/) { collecting=0; next }
            n = split($0, words)
            for (i=1; i<=n; i++) {
                if (words[i] ~ /^[a-z0-9]/ && words[i] !~ /^-/)
                    print words[i]
            }
        }
    ' "$DOCKERFILE" | sort -u
}

# Extract package names from the runner's `apt-get install -y` line.
runner_pkgs() {
    awk '
        /apt-get install -y/ {
            sub(/.*apt-get install -y[[:space:]]*/, "")
            n = split($0, words)
            for (i=1; i<=n; i++) {
                if (words[i] ~ /^[a-z0-9]/ && words[i] !~ /^-/)
                    print words[i]
            }
        }
    ' "$RUNNER" | sort -u
}

DF=$(dockerfile_pkgs)
RV=$(runner_pkgs)

# Packages the runner installs that are absent from the Dockerfile:
MISSING=$(comm -13 <(echo "$DF") <(echo "$RV"))

if [ -n "$MISSING" ]; then
    echo "FAIL: packages in remote-verify-runner.sh missing from Dockerfile:"
    echo "$MISSING" | sed 's/^/  /'
    echo "Add them to the apt-get install RUN step in scripts/exec-image/Dockerfile."
    exit 1
fi

echo "ok: all remote-verify-runner.sh apt packages are present in the Dockerfile"
exit 0

#!/usr/bin/env bash
# Rewrite the latest git commit message with custom rules

set -e

msg=$(git log -1 --pretty=%B)

msg="${msg//\\n/
}"

msg=$(echo "$msg" | grep -v "Co-authored-by")

msg=$(echo "$msg" | awk 'NF{blank=0} !NF{blank++} blank<2')

msg=$(echo "$msg" | sed '/./,$!d' | sed -e :a -e '/^\n*$/{$d;N;};/\n$/ba')

GIT_COMMITTER_DATE="$(git log -1 --pretty=format:%cI)" \
GIT_AUTHOR_DATE="$(git log -1 --pretty=format:%aI)" \
git commit --amend -m "$msg" --no-edit --no-verify
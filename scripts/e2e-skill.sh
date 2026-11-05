#!/usr/bin/env bash
# End-to-end test of the skill: copy the Rails fixture into a fresh git repo, add a realistic PR on a
# branch, install the skill, and ask headless Claude Code to "help me review this PR".
# Usage: scripts/e2e-skill.sh [workdir]   (needs ruby/bundler, claude, and a built visualdebug)
set -euo pipefail
here=$(cd "$(dirname "$0")/.." && pwd)
work=${1:-$(mktemp -d)/e2e}
bin="$here/target/release"
[ -x "$bin/visualdebug" ] || (cd "$here" && cargo build --release -q)
mkdir -p "$work" && cd "$here/fixtures/rails-app"
tar --exclude=.visualdebug --exclude=log --exclude=tmp --exclude=storage -cf - . | (cd "$work" && tar xf -)
cd "$work" && mkdir -p storage
printf '.visualdebug/\nlog/\ntmp/\nstorage/\n.bundle/\n.claude/\n' > .gitignore
git init -q -b main && git add -A && git commit -q -m "Pricing service and order pricing endpoint"
bin/rails db:migrate >/dev/null && RAILS_ENV=test bin/rails db:schema:load >/dev/null
git checkout -q -b guest-bulk-discount
cp -R "$here/scripts/e2e-pr/." .
bin/rails db:migrate >/dev/null
git add -A && git commit -q -m "Allow guest orders and add a bulk discount"
bundle exec rspec >/dev/null
PATH="$bin:$PATH" visualdebug install-skill --project
PATH="$bin:$PATH" VISUALDEBUG_NO_OPEN=1 claude -p "help me review this PR" --output-format stream-json --verbose --max-turns 80 \
  --allowedTools Skill Read Glob Grep Write Edit "Bash(visualdebug:*)" "Bash(git:*)" "Bash(bundle:*)" "Bash(bin/rails:*)" "Bash(ls:*)" "Bash(cat:*)" "Bash(mkdir:*)" \
  > transcript.jsonl
ls "$work"/.visualdebug/view-*.html && echo "transcript: $work/transcript.jsonl"

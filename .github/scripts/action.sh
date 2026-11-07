#!/usr/bin/env bash
set -euo pipefail

args=(--verbose)
if [[ -n "$CONFIG" ]]; then
  args+=(--config "$CONFIG")
fi

commands=(release pr)
if [[ -n "$COMMAND" ]]; then
  commands=("$COMMAND")
fi

prs='[]'
pr='{}'
releases='[]'
release='{}'
for command in "${commands[@]}"; do
  echo "$ git-reflow ${args[*]} $command"
  result=$(git-reflow "${args[@]}" "$command")
  result=$(printf '%s\n' "$result" | jq -ce \
    'if type == "array" then . else error("expected a JSON array") end')

  case "$command" in
    pr)
      prs=$result
      pr=$(printf '%s\n' "$prs" | jq -c '.[0] // {}')
      ;;
    release)
      releases=$result
      release=$(printf '%s\n' "$releases" | jq -c '.[0] // {}')
      ;;
  esac
done

{
  echo "prs=$prs"
  echo "pr=$pr"
  echo "releases=$releases"
  echo "release=$release"
} >> "$GITHUB_OUTPUT"

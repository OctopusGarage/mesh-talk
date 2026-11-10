#!/bin/bash

set -e

git config core.hooksPath hooks

# Ensure all hooks are executable
chmod +x hooks/*

# Check if GPG signing is configured
GPG_SIGN=$(git config commit.gpgsign || echo "")

if [ -z "$GPG_SIGN" ]; then
  git config commit.gpgsign true
fi

GPG_KEY=$(git config user.signingkey || echo "")

if [ -z "$GPG_KEY" ]; then
  echo ""
  echo "⚠️  No GPG key configured for Git."
  echo "Generate one with:"
  echo "  gpg --full-generate-key"
  echo "Then set it with:"
  echo "  git config --global user.signingkey <KEY_ID>"
fi

echo "Git hooks configured. Commits run cheap source checks."
echo "Use make verify-changed for affected validation; run ./scripts/check-health.sh explicitly for full health."

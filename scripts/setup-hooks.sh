#!/bin/bash

set -e

git config core.hooksPath hooks

# Ensure all hooks are executable
chmod +x hooks/*

# Verify that the health check script exists and is executable
if [ ! -f "scripts/check-health.sh" ]; then
    echo "Error: scripts/check-health.sh not found"
    exit 1
fi

chmod +x scripts/check-health.sh

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

# Run initial health check to verify everything is working
echo "Running initial health check..."
if ./scripts/check-health.sh; then
    echo "✅ Initial health check passed!"
else
    echo "❌ Initial health check failed!"
    echo "Please fix the issues before committing code."
    exit 1
fi

echo "✅ Git hooks have been set up successfully!"


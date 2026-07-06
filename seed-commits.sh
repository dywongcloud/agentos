#!/usr/bin/env bash
set -euo pipefail

REPO_URL="https://github.com/dywongcloud/guardian-db.git"
BRANCH="develop"
COUNT="${1:-10}"
WORKDIR="${2:-guardian-db}"

echo "Cloning repo..."
if [ ! -d "$WORKDIR/.git" ]; then
  git clone "$REPO_URL" "$WORKDIR"
fi

cd "$WORKDIR"

echo "Fetching latest..."
git fetch origin

if git show-ref --verify --quiet "refs/remotes/origin/$BRANCH"; then
  git checkout "$BRANCH"
  git pull origin "$BRANCH"
else
  echo "Remote branch '$BRANCH' does not exist. Creating it from current default branch."
  git checkout -b "$BRANCH"
fi

mkdir -p .seed-commits
SEED_FILE=".seed-commits/guardian-db-seed.log"

echo "Creating $COUNT dummy commits on $BRANCH..."

for i in $(seq 1 "$COUNT"); do
  TS="$(date -u +"%Y-%m-%dT%H:%M:%SZ")"
  UUID="$(uuidgen 2>/dev/null || cat /proc/sys/kernel/random/uuid 2>/dev/null || openssl rand -hex 16)"

  {
    echo "seed_commit=$i"
    echo "timestamp=$TS"
    echo "id=$UUID"
    echo "---"
  } >> "$SEED_FILE"

  git add "$SEED_FILE"
  git commit -m "chore(seed): add dummy commit $i"
done

echo
echo "Created $COUNT commits locally on branch '$BRANCH'."
echo "Review with:"
echo "  git log --oneline -n $COUNT"
echo
read -r -p "Push to origin/$BRANCH? [y/N] " confirm

if [[ "$confirm" =~ ^[Yy]$ ]]; then
  git push -u origin "$BRANCH"
  echo "Pushed to origin/$BRANCH."
else
  echo "Not pushed."
fi

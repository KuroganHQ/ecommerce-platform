#!/usr/bin/env bash
set -euo pipefail

die() { echo "stack: $*" >&2; exit 1; }
usage() { die "usage: stack.sh up [slot] | sync <slot> [service...] | env|e2e <slot> [playwright args] | doctor|down <slot>  (slots 3 4)"; }

cmd=${1:-}
[[ $cmd =~ ^(up|sync|env|e2e|doctor|down)$ ]] || usage
shift
slot=''
if [[ ${1:-} =~ ^[34]$ ]]; then slot=$1; shift; elif [[ $cmd != up || $# -gt 0 ]]; then usage; fi
src=$(git rev-parse --show-toplevel 2>/dev/null) || die "run it from the worktree under test"
root=${XDG_CACHE_HOME:-$HOME/.cache}/ecommerce-stack

# Host ports are 6<slot><suffix>: 63025-64901 stays above the Linux ephemeral range (32768-60999),
# so an outgoing local connection never holds a slot port as its source port.
declare -A suffix=([HTTP_PORT]=080 [HTTPS_PORT]=443 [PG_PORT]=432 [MEILI_PORT]=700 [MINIO_PORT]=900
  [MINIO_CONSOLE_PORT]=901 [MAILPIT_SMTP_PORT]=025 [MAILPIT_UI_PORT]=825 [STRIPE_MOCK_PORT]=121)

use_slot() {
  slot=$1 project=ecommerce-s$1 dir=$root/ecommerce-s$1
  log=$dir.log http=6${1}080 https=6${1}443
}
[[ -z $slot ]] || use_slot "$slot"

while read -r v; do unset "$v"; done < <(compgen -e | grep -E \
  '^(COMPOSE_|(TEST_|OWNER_)?DATABASE_URL$|APP_|PG_|MEILI_|MINIO_|MAILPIT_|STRIPE_|S3_|HTTPS?_PORT$|HOST_BIND$|E2E_|PLAYWRIGHT_|CI$)')

owner() { [[ -f $dir.owner ]] && cat "$dir.owner"; }

state() {
  local o
  o=$(owner) || { echo free; return; }
  if [[ $o == "$src" ]]; then echo mine; elif [[ -d $o ]]; then echo taken; else echo stale; fi
}

owner_check() {
  case $(state) in
    taken) die "slot $slot belongs to $(owner); use the other slot or ask its owner" ;;
    free) [[ $cmd == down ]] || die "slot $slot is free; run up first" ;;
  esac
}

pick_slot() {
  local want s
  for want in mine free stale; do
    for s in 3 4; do
      use_slot "$s"
      [[ $(state) != "$want" ]] || return 0
    done
  done
  die "slots 3 and 4 are held by $(use_slot 3; owner) and $(use_slot 4; owner)"
}

merge_env() {
  local fresh=$dir/.env.new kept
  local exprs=(-e "s/^COMPOSE_PROJECT_NAME=.*/COMPOSE_PROJECT_NAME=$project/")
  for k in "${!suffix[@]}"; do exprs+=(-e "s/^$k=.*/$k=6$slot${suffix[$k]}/"); done
  sed "${exprs[@]}" "$dir/.env.example" >"$fresh"
  if [[ -f $dir/.env ]]; then
    kept=$(awk -F= -v out="$fresh.merged" '
      /^[A-Za-z_][A-Za-z0-9_]*=/ { key = $1 } !/^[A-Za-z_][A-Za-z0-9_]*=/ { key = "" }
      NR == FNR { if (key != "") { prev[key] = $0; order[++n] = key } next }
      key != "" && key in prev { seen[key] = 1; if (prev[key] != $0) kept = kept " " key; print prev[key] > out; next }
      { print > out }
      END { for (i = 1; i <= n; i++) if (!(order[i] in seen)) { kept = kept " " order[i]; print prev[order[i]] > out }
            print kept }' "$dir/.env" "$fresh")
    mv "$fresh.merged" "$fresh"
    [[ -z $kept ]] || echo "stack: kept from the slot's .env:$kept"
  fi
  chmod 600 "$fresh"
  mv "$fresh" "$dir/.env"
}

copy() {
  mkdir -p "$dir"
  rsync -a --delete --chmod=go+rX \
    --exclude .git --exclude node_modules --exclude target --exclude .artifacts --exclude dist \
    --exclude .astro --exclude .wrangler --exclude .env --exclude .env.worktree.local \
    --exclude .cargo/worktree.toml --exclude test-results --exclude .verify --exclude .claude \
    "$src/" "$dir/"
  merge_env
  echo "$(git -C "$src" rev-parse --short HEAD)$([[ -z $(git -C "$src" status --porcelain) ]] || echo ' + uncommitted changes')" >"$dir.rev"
}

compose() { (cd "$dir" && COMPOSE_PROFILES=full docker compose -p "$project" --env-file "$dir/.env" "$@"); }

teardown() {
  if [[ -f $dir/.env ]]; then
    compose stop --timeout 5 theme-builder theme-sandbox-proxy >/dev/null 2>&1 || true
    docker ps -aq --filter "label=platform.theme-sandbox=$project" | xargs -r docker rm -f >/dev/null 2>&1 || true
    compose down -v --remove-orphans --timeout 5 >>"$log" 2>&1 || true
    docker images --format '{{.Repository}}:{{.Tag}}' --filter "reference=$project-*:local" |
      xargs -r docker rmi >/dev/null 2>&1 || true
  fi
  rm -rf "$dir" "$dir.owner" "$dir.rev"
}

case $cmd in
  up)
    mkdir -p "$root"
    exec 8>"$root/.claim.lock"
    flock 8
    if [[ -z $slot ]]; then pick_slot; fi
    case $(state) in
      taken) die "slot $slot belongs to $(owner); run up without a slot to take a free one" ;;
      stale)
        echo "stack: slot $slot belonged to $(owner), which is gone; reclaiming it"
        teardown
        ;;
    esac
    if [[ $(state) == free ]]; then
      for k in "${!suffix[@]}"; do
        ! (: <"/dev/tcp/127.0.0.1/6$slot${suffix[$k]}") 2>/dev/null || die "port 6$slot${suffix[$k]} ($k) is taken"
      done
    fi
    echo "$src" >"$dir.owner"
    exec 8>&-
    echo "stack: slot $slot"
    copy
    cd "$dir"
    echo "stack: building $project, log $log (cold: 10+ min)"
    pnpm install --frozen-lockfile --silent
    make up >>"$log" 2>&1 || die "make up failed, see $log"
    make seed >>"$log" 2>&1 || die "make seed failed, see $log"
    bash scripts/seed-m2-perf-review.sh >>"$log" 2>&1 || die "review seed failed, see $log"
    make e2e args="--project chromium-setup --no-deps" >>"$log" 2>&1 || die "e2e fixture setup failed, see $log"
    echo "stack: up in slot $slot, shop http://demo.localhost:$http https://demo.localhost:$https"
    ;;
  sync)
    owner_check
    copy
    cd "$dir"
    if (($#)); then
      compose up -d --build --wait "$@" >>"$log" 2>&1 || die "rebuild of $* failed, see $log"
      echo "stack: rebuilt $*"
    else
      make theme-build >>"$log" 2>&1 || die "theme-build failed, see $log"
      echo "stack: theme and checkout artifacts rebuilt and published"
    fi
    ;;
  env)
    grep -E '^(COMPOSE_PROJECT_NAME|HTTPS?_PORT|PG_PORT|MAILPIT_UI_PORT|E2E_RATE_SECRET)=' "$dir/.env" | sed 's/^/export /'
    ;;
  e2e)
    [[ -f $dir/.env ]] || die "no stack in slot $slot; run up first"
    set -a
    # shellcheck disable=SC1091
    source "$dir/.env"
    set +a
    cd "$src"
    exec pnpm --filter @platform/e2e exec playwright test "$@"
    ;;
  doctor)
    [[ -f $dir.owner ]] || die "slot $slot is free"
    echo "owner:   $(owner)$(case $(state) in taken) echo '  (NOT this worktree: do not drive it)' ;; stale) echo '  (gone: up reclaims the slot)' ;; esac)"
    echo "copied:  $(<"$dir.rev") at the last up/sync; worktree HEAD is now $(git -C "$src" rev-parse --short HEAD)"
    compose ps --format '{{.Service}}\t{{.State}}\t{{.Health}}' | sort
    for u in "http://api.localhost:$http/readyz" "http://demo.localhost:$http/" "https://demo.localhost:$https/"; do
      printf '%s %s\n' "$(curl -sk -o /dev/null -w '%{http_code}' --max-time 10 "$u" || true)" "$u"
    done
    ;;
  down)
    owner_check
    teardown
    echo "stack: $project down (log kept at $log)"
    ;;
esac

PATH_add() {
  local p
  for p in "$@"; do
    [ "${p#/}" = "$p" ] && p="$PWD/$p"
    export PATH="$p:$PATH"
  done
}
path_add() {
  local var="$1"; shift
  local p
  for p in "$@"; do
    [ "${p#/}" = "$p" ] && p="$PWD/$p"
    eval "export $var=\"$p:\${$var}\""
  done
}
PATH_rm() {
  local new_path p pattern
  for pattern in "$@"; do
    new_path=
    local IFS=:
    for p in $PATH; do
      case "$p" in
        $pattern) ;;
        *) new_path="${new_path:+$new_path:}$p" ;;
      esac
    done
    export PATH="$new_path"
  done
}
path_rm() {
  local var="$1"; shift
  local new_path p pattern val
  for pattern in "$@"; do
    eval "val=\$$var"
    new_path=
    local IFS=:
    for p in $val; do
      case "$p" in
        $pattern) ;;
        *) new_path="${new_path:+$new_path:}$p" ;;
      esac
    done
    eval "export $var=\"\$new_path\""
  done
}
MANPATH_add() {
  local p
  for p in "$@"; do
    [ "${p#/}" = "$p" ] && p="$PWD/$p"
    export MANPATH="$p${MANPATH:+:$MANPATH}"
  done
}
has() { command -v "$1" >/dev/null 2>&1; }
watch_file() { :; }
watch_dir() { :; }
expand_path() {
  case "$1" in
    ~/*) echo "$HOME/${1#~/}" ;;
    /*)  echo "$1" ;;
    *)   echo "$PWD/$1" ;;
  esac
}
find_up() {
  local file="$1" dir="$PWD"
  while [ "$dir" != "/" ]; do
    if [ -e "$dir/$file" ]; then echo "$dir/$file"; return 0; fi
    dir="${dir%/*}"
    [ -z "$dir" ] && dir="/"
  done
  return 1
}
env_vars_required() {
  local var _v rc=0
  for var in "$@"; do
    eval "_v=\"\${$var-}\""
    [ -n "$_v" ] || { log_error "$var is required"; rc=1; }
  done
  unset _v
  return $rc
}
load_prefix() {
  local p="${1%/}"
  [ "${p#/}" = "$p" ] && p="$PWD/$p"
  PATH_add "$p/bin" "$p/sbin"
  [ -d "$p/include" ]    && path_add CPATH "$p/include"
  [ -d "$p/lib" ]        && path_add PKG_CONFIG_PATH "$p/lib/pkgconfig" \
                         && path_add LIBRARY_PATH "$p/lib" \
                         && path_add DYLD_LIBRARY_PATH "$p/lib" \
                         && path_add LD_LIBRARY_PATH "$p/lib"
  [ -d "$p/lib64" ]      && path_add PKG_CONFIG_PATH "$p/lib64/pkgconfig" \
                         && path_add LIBRARY_PATH "$p/lib64" \
                         && path_add DYLD_LIBRARY_PATH "$p/lib64" \
                         && path_add LD_LIBRARY_PATH "$p/lib64"
  [ -d "$p/share/man" ]  && MANPATH_add "$p/share/man"
  return 0
}
source_env() { [ -f "$1" ] && . "$1"; }
source_env_if_exists() { [ -f "$1" ] && . "$1" || :; }
source_up() {
  local d="$PWD"
  while d="$(dirname "$d")" && [ "$d" != "/" ]; do
    if [ -f "$d/.envrc" ]; then . "$d/.envrc"; return; fi
  done
}
source_up_if_exists() { source_up 2>/dev/null || :; }
dotenv() {
  local f="${1:-.env}"
  [ -f "$f" ] || return 1
  set -a; . "$f"; set +a
}
dotenv_if_exists() { dotenv "${1:-.env}" 2>/dev/null || :; }
log_status() { echo "denv: $*" >&2; }
log_error() { echo "denv: error: $*" >&2; }
strict_env() { set -euo pipefail; }
unstrict_env() { set +euo pipefail; }

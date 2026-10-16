# Source from ~/.zshrc to enable `start apiosk` in any directory.
function start() {
  if [[ "$#" -ne 1 || "$1" != apiosk ]]; then
    print -u2 -- 'Usage: start apiosk'
    return 2
  fi
  command node /Users/olivier/Documents/Apiosk/Production/Main/gateway/scripts/dev-local.mjs --execute --recurring --no-open
}

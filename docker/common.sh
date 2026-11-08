# Sourced by the scripts that run inside the test containers. scripts/docker-test.sh mounts
# the repository read-only at /src, the host's crate downloads at /hostreg and a folder for
# logs at /out.

failed=()

# Copies the repository to /work, without build output or Git data.
copy_source() {
  find /work -mindepth 1 -delete
  (cd /src && tar --exclude=./target --exclude=./.git --exclude=./out --exclude=./dist -cf - .) |
    (cd /work && tar -xf -)
  # Cargo goes by modification times: a file put back to an older version must still count
  # as changed.
  find /work -type f -exec touch {} +
}

# Brings the crate downloads up to date from the host, so that cargo can run offline.
copy_registry() {
  mkdir -p "$CARGO_HOME/registry"
  cp -R --preserve=timestamps --update /hostreg/index /hostreg/cache "$CARGO_HOME/registry/"
}

# step NAME COMMAND...: runs a check with its output in /out/NAME.log, and goes on to the next
# one whether it passed or not.
step() {
  local name=$1 start=$SECONDS
  shift
  echo "--- $name"
  if "$@" >"/out/$name.log" 2>&1; then
    echo "--- $name: passed ($((SECONDS - start))s)"
  else
    echo "--- $name: FAILED ($((SECONDS - start))s); the end of $name.log:"
    tail -n 40 "/out/$name.log" | sed 's/^/    /'
    failed+=("$name")
  fi
}

# Says which checks failed, and fails if any did.
finish() {
  if [ "${#failed[@]}" -gt 0 ]; then
    echo "--- $1: failed: ${failed[*]}"
    exit 1
  fi
  echo "--- $1: all checks passed"
}

# The same command line checks as continuous integration, with BIN the program to run.
cli_smoke() {
  local bin=$1 code=0
  "$bin" --version
  "$bin" --list-languages
  "$bin" --cli samples/left/same.txt samples/right/same.txt
  "$bin" --cli -r samples/left samples/right >/dev/null || code=$?
  [ "$code" -eq 1 ] || { echo "expected exit code 1 for differing folders, got $code"; return 1; }
}

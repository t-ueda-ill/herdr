# Docker testing and debugging

Build the Linux test environment from the repository root:

```sh
docker build -f Dockerfile.test -t herdr-test:local .
docker run --rm --init herdr-test:local
```

The default command is `just test`, including nextest, maintenance, integration
asset, architecture, and documentation contract tests. The image includes the
repository's Rust toolchain, Zig 0.16.0, Bun 1.3.14, Node.js 24, Python, just,
cargo-nextest, GDB, strace, and an SSH client. Source is copied into the image;
rebuild after editing it. No host Docker socket or Herdr session is mounted.

Run Linux lint and CI checks, documentation checks, or a focused test:

```sh
docker run --rm --init herdr-test:local just ci
docker run --rm --init herdr-test:local just docs-contract-test
docker run --rm --init herdr-test:local just test-one container
```

For repeated local runs, keep build products in a named volume:

```sh
docker run --rm --init -v herdr-test-cache:/var/cache/herdr herdr-test:local just test
```

Open an interactive debugger shell:

```sh
docker run --rm --init -it --cap-add=SYS_PTRACE --security-opt seccomp=unconfined \
  -v herdr-test-cache:/var/cache/herdr herdr-test:local bash
cargo build --locked
gdb --args /var/cache/herdr/target/debug/herdr --help
strace -f /var/cache/herdr/target/debug/herdr --help
```

Debugger permissions are only needed for tracing. Run ordinary tests without
them. The container has its own filesystem and session sockets, so debugging
does not connect to a host Herdr session.

`just check` also performs Windows cross linting. This Linux image does not
include the separately licensed Windows SDK; use the documented
`just setup-windows-cross` procedure if that check is needed. Linux `just ci`
and `just test` do not validate native Windows behavior.

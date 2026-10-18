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

`COPY` keeps the host modification times, and cargo treats a source as fresh
when its build product is newer. A container build that ran after an edit was
saved on the host can therefore hide that edit in the cached products. Before
testing an edit against a reused cache, drop the crate's products:

```sh
docker run --rm --init -v herdr-test-cache:/var/cache/herdr herdr-test:local \
  bash -c 'cargo clean -p herdr && cargo clean -p herdr --release && just test'
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

`just check` also performs Windows cross linting, which needs Microsoft's
separately licensed Windows SDK. The image ships `xwin` and the
`x86_64-pc-windows-msvc` target but never downloads the SDK by itself. Download
it once into a dedicated volume, accepting the SDK license explicitly, then
reuse that volume for every `just windows-lint` or `just check` run:

```sh
docker run --rm --init -v herdr-windows-cross:/root/.local/share/herdr/windows-cross \
  herdr-test:local just setup-windows-cross --accept-license
docker run --rm --init -v herdr-test-cache:/var/cache/herdr \
  -v herdr-windows-cross:/root/.local/share/herdr/windows-cross \
  herdr-test:local just check
```

A passing Windows cross lint proves the Windows target compiles and is clippy
clean. It does not run anything on Windows; Linux `just ci`, `just test`, and
`just check` do not validate native Windows behavior.

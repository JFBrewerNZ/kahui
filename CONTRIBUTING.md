# Contributing to Kāhui

Thanks for looking. Kāhui is a young project and the shape of it is still being
decided, so opinions are as welcome as patches.

## The one constraint

**Nothing may require infrastructure that anyone controls.** No cloud service, no
backend, no login server, no bootstrap node that has to be up, no default that points at
a company's hardware. If a change means "and then it contacts X", it is the wrong change,
however convenient X is.

This is not a stylistic preference. It is the entire claim the project makes: that if the
Kāhui developers vanished tomorrow, existing communities would keep running. Every
dependency and every design decision gets measured against that.

The most notable casualty so far: [iroh](https://www.iroh.computer/) has meaningfully
better NAT traversal than libp2p, and was still set aside, because its reliability comes
partly from relays somebody operates. See `docs/architecture.md` §9.

## Getting set up

```bash
cargo test --workspace     # 73 tests, a few seconds
bash scripts/demo.sh       # three real nodes, ~40 seconds
```

`scripts/demo.sh` is the one to run when you have changed anything about networking or
sync. It starts three processes with three databases, kills one mid-conversation, brings
it back, and fails loudly if the community did not survive.

## Before you open a pull request

```bash
cargo fmt --all
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```

CI runs all three on Linux, Windows and macOS, plus the demo on Linux.

## What good looks like here

**Tests should describe behaviour, not implementation.** `a_community_outlives_the_node_that_created_it`
tells you what broke; `test_sync_3` does not.

**Test the promise, not the implementation's current habits.** An early version of the
acceptance test asserted that messages appeared in the order they were typed. They do
not, and cannot: concurrent messages are ordered by event id, which is identical on every
node but unrelated to wall clock. The test was wrong, not the code. If you find yourself
asserting something the protocol does not actually guarantee, the test is the bug.

**Comments explain why, not what.** The code already says what it does.

**Layering is load-bearing.** `kahui-proto` has no IO and must keep it that way — a
WebAssembly client shares that crate verbatim, which is what makes it the same network
rather than a lookalike. `kahui-store` is a trait because storage is the one layer that
genuinely cannot be shared. Nothing above `kahui-node` should know what gossipsub is.

## Adding a dependency

Check its licence is compatible with AGPL-3.0-or-later. Everything in the tree today is
permissive (MIT, Apache-2.0, BSD, MPL-2.0, Zlib); please keep it that way.

Then check it against the constraint at the top. A crate that phones home, or that only
works well with a hosted service, does not belong here no matter how good it is.

## Changing the wire format

`kahui-proto` defines what goes on the wire, and signatures are computed over the
canonical encoding of an `Event`. Changing that struct changes every signature, so:

- Adding a `Payload` variant is backwards compatible for readers that skip unknown
  payloads.
- Changing an existing variant, or any field of `Event`, is not, and needs a
  `PROTOCOL_VERSION` bump.

There is no upgrade machinery yet. Until there is, treat the format as expensive to
change and worth getting right.

## Security

Please do not open a public issue for anything exploitable. Email the maintainer instead
and give us a chance to fix it first.

Worth knowing before you report: events are signed but **not encrypted** — every member
can read all history, by design. Transport is encrypted (libp2p Noise), and end-to-end
group encryption is a known gap, not an oversight. `docs/architecture.md` §10 lists the
rest of the honest limitations.

## Licensing of contributions

By contributing you agree your work is licensed under **AGPL-3.0-or-later**, the same as
the project.

There is deliberately **no contributor licence agreement**. A CLA would let the project
later sell commercial exceptions, and that option is being given up on purpose: it would
mean contributors signing over rights that the maintainer keeps and they do not. If that
ever changes it will be discussed in the open first, and it cannot be applied
retroactively to work already contributed under these terms.

# Guided review

A guided session lets an educator walk a group through a teaching case: one
person controls the view, everyone else follows on their own copy of the same
de-identified images, and only the view state crosses the network.

Ruthos ships with no server configured, and no server address appears in its
code or default settings. A session relay is reached through an invitation, a
profile, a site's `ruthos.ini`, or an address typed under Enter address manually.

## Joining

Click the invitation link from your educator. Ruthos shows the server's domain
in large type and asks you to confirm. If you have not signed in to that
server, sign-in opens in your browser, where the address bar shows the real
domain. Ruthos never sees your password.

## Setting up a room

| You want | Use |
| --- | --- |
| One person to join one session | An invitation link |
| A class's settings on their own laptops | A signed `.ruthosprofile`, or a zip of Ruthos with a `ruthos.ini` beside `ruthos.exe` |
| A managed lab that cannot be repointed | The administrator's policy file |

`ruthos.ini` is described in [ruthos.ini.example](../ruthos.ini.example). It
sets the default server and optional allow lists, holds no credentials, and is
git-ignored.

The policy file is `%ProgramData%\Ruthos\policy.json` on Windows or
`/Library/Application Support/Ruthos/policy.json` on macOS, writable only by
administrators. What it sets is locked and shown with a padlock:

```json
{
  "server": "https://relay.example.org/teaching",
  "servers": ["https://relay.example.org/teaching"],
  "issuers": ["https://relay.example.org/teaching"]
}
```

## What a profile can change

A profile can set the session server, a session to join, cases to fetch, a
measurements library and a few viewer preferences. It can never set
credentials, the DICOM listener, PACS nodes, routing, folders, a proxy or
anything that runs code. A file with one key outside the list is rejected
whole. Every change appears in a review dialog, nothing applies until Apply,
and Revert restores the previous values.

A signature proves who issued a file and that nobody has altered it. It does
not make the issuer trustworthy, which is why the list above is short and the
domain is shown in large type.

## What leaves your machine

Only a teaching copy can be shared: a viewer showing any other study publishes
nothing, because its identifiers are real. In a session the relay receives the
view state (UIDs of the teaching copy, windows, positions, measurements) and
the names of the people taking part. Tokens stay in the operating system's
credential store, and nothing received from other participants is written to
disk.

## For developers

The protocol, sequence recovery and connection handling are in
`crates/ruthos-session`; `crates/ruthos-ui/src/session.rs` and
`session_view.rs` connect it to the windows. `tests/relay.rs` drives the client
against a scripted relay on a loopback port. `tests/live.rs` runs against a real
relay when a git-ignored `ruthos.local.ini` names one under `[dev]`, and does
nothing otherwise. Keep server addresses out of the repository.

# Security policy

CCP servers sit between an AI model and your data, so security reports are taken seriously.

## Reporting a vulnerability

**Please do not open a public issue.** Use GitHub's private reporting:
[Report a vulnerability](https://github.com/hacimertgokhan/ccp/security/advisories/new).
Include the server and version, how to reproduce it, and the impact you expect.

You can expect an acknowledgement within a few days and a fix or a mitigation plan as soon as it is
confirmed. Reporters are credited in the release notes unless they prefer otherwise.

## In scope

* SQL injection or any way caller text reaches SQL as something other than a bound value
* Writing to a database through a CCP server
* Credentials appearing in output, errors or logs
* Resource exhaustion that bypasses the documented limits (rows, relation rows, depth)

## Design guarantees

Servers are read-only, take identifiers only from the database catalog, bind every value,
cap result sizes, and scrub secrets from errors. Connections are currently plain TCP (TLS is
planned): use localhost, a tunnel or a private network, and a dedicated read-only database user.

## Supported versions

Only the latest release receives fixes while the protocol is pre-1.0.

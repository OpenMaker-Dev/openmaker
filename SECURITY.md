# Security policy

## Reporting a vulnerability

Report vulnerabilities privately through GitHub: open the repository's
**Security** tab and choose **Report a vulnerability**. Please do not open a
public issue, pull request or discussion for a security problem.

Include what you found, the affected crate or component, the commit you
tested, and steps or a file that reproduce it. Do not attach real
credentials, license keys or files that hold real people's data; a small
synthetic file that triggers the problem is best.

A maintainer replies in the private advisory, keeps you informed while a fix
is prepared, and credits you in the advisory unless you ask not to be named.
There is no bug bounty.

## Supported versions

OpenMaker is pre-alpha and has no releases yet. Fixes go to the `main`
branch only.

Do not use OpenMaker with data you cannot afford to lose, and do not expose
OpenMaker Server (`om-server`) or its Admin Console (`om-admin`) to networks
you do not trust.

## Scope

In scope: OpenMaker's own code in this repository, in particular

- the network-facing parts: OpenMaker Server, its Admin Console, and the
  client's connections to FileMaker Server and OpenMaker Server;
- parsing of `.fmp12` files, layouts, calculations, scripts and protocol
  messages, which must treat their input as hostile (crashes, unbounded
  memory or recursion, and out-of-bounds reads count);
- credential and session handling.

Out of scope: vulnerabilities in Claris FileMaker products themselves
(report those to Claris), and problems that need an attacker who already
controls the machine OpenMaker runs on.

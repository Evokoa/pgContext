# Security Policy

pgContext 0.2.0 is prepared for operator-controlled GitHub publication.
Experimental surfaces remain outside the stable compatibility promise.

## Supported Versions

Security fixes currently apply to the `master` branch and, after publication, the
latest `0.2.x` release. PostgreSQL 17 is the only supported release target.

## Reporting a Vulnerability

Do not open a public issue for a suspected vulnerability. Report it privately
to [team@evokoa.com](mailto:team@evokoa.com). Include the affected commit or
version, a concise reproduction, expected impact, and any available mitigation.
The maintainers will acknowledge receipt and coordinate disclosure and fixes
through the reporting address.

Security-sensitive areas include filter JSON parsing, SQL predicate rendering,
identifier quoting, SPI parameter binding, ACL/RLS checks, mmap validation, and
PostgreSQL access method callbacks.

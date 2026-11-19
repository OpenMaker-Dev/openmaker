# Test fixtures

`v1-client.pem` / `v1-client.key`: a throwaway self-signed X.509 version 1
RSA-2048 client certificate and its key, generated for tests only (FileMaker
Pro presents a version 1 certificate). Not used anywhere else.

`tasks.rs`: builds the small hosted file the Perform Script on Server tests
run against (table Tasks, its records, a layout, an account and scripts),
written with `om_hbam::writer`; included by `tests/psos.rs`, the unit tests
of `src/scripts` and `om-provider/tests/psos.rs`.

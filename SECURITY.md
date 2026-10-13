# Security policy

## Reporting a vulnerability

Please email andy@dixon.cx with a description and, if possible, a minimal
reproduction. You will receive an acknowledgement within a week. Please do not
open a public issue for security problems until a fix is available.

## Threat model

Apex treats the following as untrusted input:

* file contents and filesystem metadata, including symlinks and hard links;
* project and workspace configuration files found in opened folders;
* output from any external process (build tasks, language servers);
* clipboard contents and drag-and-drop payloads.

Apex never executes project-provided commands, language servers, `git`
or build tasks in a folder without explicit per-workspace trust.
Configuration is data and cannot execute code. Apex makes no network
connections of its own; `git` and the GitHub CLI reach the network only
when you ask (fetch, pull, push, pull requests) or turn on auto-fetch.

Details of the file-handling guarantees are in `docs/security/file-handling.md`.

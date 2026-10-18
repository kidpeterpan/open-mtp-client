# Security policy

## Reporting a vulnerability

Please **don't open a public issue** for a security problem. Use GitHub's private reporting instead:
**Security › Report a vulnerability** on the repository page.

Include what you found, how to reproduce it, and the version (Help › About). You'll get a reply as soon as
the maintainer can manage; this is a one-person project, so please be patient.

## What counts

The app treats the phone as untrusted input and your files as yours. Reports in these areas are the most
useful:

- a malformed MTP reply that crashes the app, hangs it or makes it use unbounded memory
- a file name or path from the phone that writes outside the folder the user chose
- anything that makes the app send data off the machine (it has no network code of its own)
- a way to make the app delete or overwrite a file the user didn't ask it to

## Supported versions

Only the latest release and `main` get fixes.

# Extra certificate authorities for the build

Behind a proxy that inspects TLS (Zscaler, Netskope, a corporate firewall…), downloads made during the
build fail with `self-signed certificate in certificate chain` or `UnknownIssuer`: the proxy re-signs
the sites with a company root that the Linux images do not know.

Put that root (and its intermediates if needed) here as PEM files with the `.crt` extension. The build
adds them to the trusted authorities of the stages that download something, and of the final image.

- On Windows, `.\scripts\build.ps1 -TrustWindowsCa "<pattern>"` exports the valid certificates of the
  Windows store whose subject matches the pattern (for example `-TrustWindowsCa "Zscaler"`) into
  `certs/windows-ca.crt`.
- On Linux or macOS, copy the company root from the system store or ask your IT team for it.

Everything in this folder but this file is ignored by git: these certificates belong to one machine
or one company and are never committed.

---
'@reddb-io/client': patch
'@reddb-io/sdk': patch
---

Fix `reds://` and `grpc(s)://` connection strings dropping userinfo and query params: `reds://user:pass@host:5050` parsed host as `user` and port as `NaN`, so credentials never reached the server and the HTTPS auto-login leg never fired. The legacy branch now decodes userinfo, host, port and `token`/`apiKey`/`loginUrl` with standard URL rules; bare-host shapes keep their previous behaviour.

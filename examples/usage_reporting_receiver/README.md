# KeepPeek Usage Reporting Receiver

Publishable reference receiver for KeepPeek opt-in usage reporting (`schemas/usage-reporting/v1.schema.json`).

This binary is separate from the `keeppeek` NVR binary. It stores at most one latest validated row per `installation_id`, never persists source IP addresses or derived location, expires every raw row and deletion tombstone within **60 days**, and publishes only **aggregate** JSON where every cell represents at least **10** distinct installations with complementary suppression against differencing.

## HTTP contract

| Method & Path                                | Authorization                                | Behavior                                                                                                                                                                                                                                                                     |
| -------------------------------------------- | -------------------------------------------- | ---------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `PUT /v1/installations/{installation_id}`    | `Authorization: Bearer <installation_token>` | Validates `application/json` body (`<= 65,536` bytes) against closed v1 schema, verifies per-installation proof token (SHA-256, constant time), enforces monotonic `report_sequence`, rejects tombstoned IDs (`410 Gone`), and replaces the single row with a 60-day expiry. |
| `DELETE /v1/installations/{installation_id}` | `Authorization: Bearer <installation_token>` | Idempotently deletes any stored row for `installation_id` and records a bounded 60-day deletion tombstone so stale in-flight `PUT`s cannot resurrect the row. Returns `204 No Content`.                                                                                      |
| `GET /v1/aggregates`                         | Public                                       | Purges expired rows and returns the allowlisted public aggregate JSON (`k >= 10` per cell plus complementary suppression). Contains zero UUIDs and zero raw rows.                                                                                                            |
| `GET /v1/installations/{installation_id}`    | Denied (`405`)                               | Raw per-installation rows are never readable over HTTP.                                                                                                                                                                                                                      |
| `GET /v1/admin/status`                       | `Authorization: Bearer <admin_token>`        | Operator-only aggregate row counts (`active_installations`, `active_tombstones`, `expired_installations`). Disabled unless `--admin-token-env` is configured.                                                                                                                |
| `POST /v1/admin/expire`                      | `Authorization: Bearer <admin_token>`        | Operator-only manual trigger for the 60-day expiry sweep.                                                                                                                                                                                                                    |

## Build and test

```bash
cargo test --locked -p usage-reporting-receiver
```

Export a static public aggregate file from a receiver database:

```bash
cargo run --locked -p usage-reporting-receiver -- \
  --database /var/lib/keeppeek-usage-receiver/receiver.db \
  --export-aggregates /var/www/usage-aggregates/v1.json
```

## Production deployment requirements

Before pointing live KeepPeek installations at a hosted receiver:

1. **TLS termination**: Deploy behind a TLS reverse proxy (TLS 1.2+, valid public certificate). Do not expose plaintext HTTP.
2. **Disable source-IP logging**: Configure the reverse proxy and CDN to **disable access logs** or strip `$remote_addr`, `X-Forwarded-For`, `Forwarded`, and geo-IP headers completely before logging or forwarding. Example Nginx block:
   ```nginx
   server {
       listen 443 ssl http2;
       access_log off;
       error_log /var/log/nginx/usage-receiver-error.log crit;
       client_max_body_size 64k;
       location /v1/ {
           proxy_pass http://127.0.0.1:8091;
           proxy_set_header X-Forwarded-For "";
           proxy_set_header X-Real-IP "";
           proxy_set_header Forwarded "";
       }
   }
   ```
3. **Retention & deletion ownership**: The receiver runs an automatic hourly expiry sweep deleting rows and tombstones past 60 days, and also sweeps before computing `/v1/aggregates`. Operators should additionally monitor `GET /v1/admin/status` (`expired_installations == 0`) with a dedicated rotated admin token.
4. **Public aggregate publication**: Publish the output of `GET /v1/aggregates` (or `--export-aggregates`) at the documented public URL. Never expose the SQLite database file, backups containing raw rows, or admin credentials publicly.
5. **Representative output**: See [`public/aggregate-example.json`](public/aggregate-example.json).

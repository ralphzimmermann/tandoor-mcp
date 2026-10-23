# S3 thumbnails and inline galleries

This is the implementation and operations document for the S3-backed recipe
thumbnail cache and recipe/meal-plan MCP Apps galleries. Keep the status
section current when the feature changes so another developer or AI agent can
continue safely.

## Status

- [x] Configuration and startup validation
- [x] Bounded thumbnail generation
- [x] S3 and in-memory storage implementations
- [x] Recipe gallery, meal-plan gallery, and explicit image tools
- [x] Recipe and meal-plan MCP Apps resources and inline widgets
- [x] Protocol, service, and image tests
- [x] Compose/MinIO development configuration
- [x] AWS and MinIO operating instructions
- [x] Local and Docker verification log completed

## Data flow

`get_recipe_gallery` and `get_meal_plan_gallery` look up each recipe and derive
an object key from the recipe ID, the photo path without its temporary query
string, and the thumbnail edge size:

```text
thumbnails/{recipe_id}/{sha256(stable-image-path)}-{edge}.jpg
```

On a cache miss the MCP server retrieves the authoritative image through the
existing authenticated/SSRF-safe path, bounds decoding to 12000×12000 and
256 MiB, applies EXIF orientation, resizes to a 512 px longest edge, flattens
transparency onto white, strips metadata, and stores a quality-80 JPEG. At most
four thumbnails are generated concurrently.

The server signs the private object for 24 hours and returns its URL only in
tool-result `_meta`, which is delivered to the MCP App but hidden from the
model. This prevents long bearer URLs from leaking into or being corrupted by
model-generated Markdown. Each widget merges that private metadata with the
model-safe structured content and refreshes expired URLs. Gallery results carry
no inline image bytes: a ~500 KB result with base64 thumbnails was never handed
to the component by ChatGPT (see `docs/mcp.md`). The meal-plan variant keeps the
date, meal type, title, recipe summary, tags, note, timing, and servings in the
same component. Individual photos, including the original, remain available
through `get_recipe_image`.

Changing or deleting a recipe photo attempts immediate cache invalidation. A
content-derived key and the lifecycle policy also make stale objects harmless.

The service keeps an in-memory map of objects it has seen, but trusts an entry
for only ten minutes before repeating `HEAD`; objects removed underneath the
server (lifecycle expiry, manual deletion) are therefore regenerated on the
next request after that window, and a failed object read evicts the entry
immediately. Presigned URLs appear only in gallery `_meta`; `get_recipe_image`
and `get_recipe_details` return the photo as an inline image block and never
place the URL in model-visible text.

## Environment variables

See `.env.example`. `S3_BUCKET` enables the feature; leaving it unset preserves
the server's non-gallery behavior.

| Variable | Default | Meaning |
| --- | --- | --- |
| `S3_BUCKET` | unset | Private thumbnail bucket |
| `S3_REGION` | `us-east-1` | Bucket region |
| `S3_ENDPOINT` | AWS | Internal S3-compatible API endpoint |
| `S3_ACCESS_KEY_ID` | required | Least-privilege access key |
| `S3_SECRET_ACCESS_KEY` | required | Secret key |
| `S3_FORCE_PATH_STYLE` | `false` | Required by MinIO and some compatible stores |
| `S3_THUMBNAIL_PREFIX` | `thumbnails/` | Object-key prefix |
| `S3_PUBLIC_BASE_URL` | derived | Optional browser-visible endpoint used when signing URLs and in widget CSP |
| `THUMBNAIL_URL_TTL_SECS` | `86400` | Presigned URL lifetime, 60–604800 seconds |
| `THUMBNAIL_MAX_EDGE` | `512` | Longest edge, 128–2048 px |

`S3_ENDPOINT` may be an internal container address while
`S3_PUBLIC_BASE_URL` is browser-visible. The S3 client uses the former for
object operations and a separate client configured with the latter only for
signing. Both addresses must reach the same S3 service. Do not rewrite a
presigned URL after signing because its host participates in the signature.

## AWS S3 setup

The commands use `eu-central-1`; substitute the deployment region and a
globally unique bucket name.

```bash
export BUCKET=tandoor-mcp-thumbnails-example
export REGION=eu-central-1

aws s3api create-bucket \
  --bucket "$BUCKET" \
  --region "$REGION" \
  --create-bucket-configuration LocationConstraint="$REGION"

aws s3api put-public-access-block \
  --bucket "$BUCKET" \
  --public-access-block-configuration \
  BlockPublicAcls=true,IgnorePublicAcls=true,BlockPublicPolicy=true,RestrictPublicBuckets=true

aws s3api put-bucket-encryption \
  --bucket "$BUCKET" \
  --server-side-encryption-configuration \
  '{"Rules":[{"ApplyServerSideEncryptionByDefault":{"SSEAlgorithm":"AES256"}}]}'
```

For `us-east-1`, omit `--create-bucket-configuration`.

Create an IAM policy restricted to this bucket and prefix:

```json
{
  "Version": "2012-10-17",
  "Statement": [
    {
      "Sid": "ListThumbnailPrefix",
      "Effect": "Allow",
      "Action": ["s3:ListBucket"],
      "Resource": ["arn:aws:s3:::tandoor-mcp-thumbnails-example"],
      "Condition": {
        "StringLike": {
          "s3:prefix": ["thumbnails", "thumbnails/*"]
        }
      }
    },
    {
      "Sid": "ManageThumbnailObjects",
      "Effect": "Allow",
      "Action": ["s3:GetObject", "s3:PutObject", "s3:DeleteObject"],
      "Resource": ["arn:aws:s3:::tandoor-mcp-thumbnails-example/thumbnails/*"]
    }
  ]
}
```

Attach it to a dedicated IAM user or workload role. Put its credentials in the
runtime secret manager, not in a committed env file. Static IAM-user
presigned URLs support at most seven days. URLs signed with temporary STS
credentials stop working when those credentials expire, commonly after
12 hours, even if a longer URL TTL was requested.

At startup the server tests access with a one-object `ListObjectsV2` request
scoped to `S3_THUMBNAIL_PREFIX`. It intentionally does not use `HeadBucket`,
which requires broader bucket-level permission and returns 403 for the
least-privilege policy above even when thumbnail reads and writes work.

Plain `<img>` loading does not require bucket CORS. Add a narrowly scoped CORS
rule only if a later widget reads image bytes using JavaScript.

### Lifecycle policy

Ninety days balances storage cleanup against regeneration. S3 expiration is
based on object creation, not last access, so an actively viewed recipe
generates one small replacement about once per quarter.

Save this as `thumbnail-lifecycle.json` and apply it:

```json
{
  "Rules": [
    {
      "ID": "ExpireRecipeThumbnails",
      "Status": "Enabled",
      "Filter": {"Prefix": "thumbnails/"},
      "Expiration": {"Days": 90},
      "AbortIncompleteMultipartUpload": {"DaysAfterInitiation": 7},
      "NoncurrentVersionExpiration": {"NoncurrentDays": 7}
    }
  ]
}
```

```bash
aws s3api put-bucket-lifecycle-configuration \
  --bucket "$BUCKET" \
  --lifecycle-configuration file://thumbnail-lifecycle.json
```

Remove `NoncurrentVersionExpiration` when bucket versioning is not enabled.

### Deployed AWS resources

The production thumbnail cache was provisioned in `us-west-2` on 2026-10-22:

- private bucket `recipe-thumbnails.thehealthwarriors.net`;
- IAM user `tandoor-mcp-thumbnails`;
- managed policy `TandoorMcpRecipeThumbnails`, restricted to the
  `thumbnails/` prefix; and
- the 90-day expiration and seven-day incomplete-upload cleanup described
  above.

The dedicated access key is stored only in the ignored deployment `.env`.
Rotate that key in IAM and update the deployment environment together.

## Local MinIO

The `s3-dev` Compose profile creates the bucket and installs the same 90-day
expiry using the AWS CLI. The former upstream MinIO container repositories were
withdrawn in 2026, so Compose uses the maintained Chainguard development image.
Use development credentials, not AWS credentials:

```bash
S3_BUCKET=tandoor-mcp-thumbnails \
S3_REGION=us-east-1 \
S3_ENDPOINT=http://minio:9000 \
S3_PUBLIC_BASE_URL=http://localhost:9000 \
S3_ACCESS_KEY_ID=minioadmin \
S3_SECRET_ACCESS_KEY=minioadmin123 \
S3_FORCE_PATH_STYLE=true \
docker compose --profile chatgpt --profile s3-dev up --build
```

The MinIO console is at `http://localhost:9001`. These localhost URLs support
local browser/widget development only; OpenAI's hosted ChatGPT cannot reach
them. Use AWS S3 or another publicly reachable HTTPS S3-compatible endpoint
for an end-to-end ChatGPT test.

Run the ignored storage integration test while MinIO is available:

```bash
cargo test storage::s3::tests::minio_round_trip -- --ignored
```

## Security and operations

- The bucket remains private. A presigned URL authorizes one thumbnail until
  it expires; it does not expose credentials or other objects.
- The widget CSP allows only the configured S3 origin.
- The Tandoor token is never sent to S3 or the browser.
- Generation failures are reported per recipe; they do not substitute photos.
- Monitor thumbnail generation warnings, S3 `4xx/5xx` responses, object count,
  and bucket bytes.

## CloudFront

CloudFront is intentionally not part of the first deployment. S3 already
serves these small, low-volume objects efficiently. The storage abstraction
and `S3_PUBLIC_BASE_URL` leave room for it if usage becomes multi-region,
stable branded image URLs are required, or the S3 presign lifetime becomes a
problem. A CloudFront version should use Origin Access Control and CloudFront
signed URLs; it must not make the bucket public.

## Verification log

Completed on 2026-10-22:

- formatting and strict Clippy passed;
- unit, protocol, and Docker-backed Tandoor integration tests passed;
- `cargo audit` passed with no vulnerabilities (the AWS SDK currently carries
  one non-failing `lru` unsoundness warning without a patched release);
- locked release and production Docker builds passed;
- the MinIO bucket/lifecycle initializer and ignored S3 round-trip test passed;
- a gallery-enabled server returned structured gallery content and the MCP Apps
  resource with `text/html;profile=mcp-app` and the expected CSP origin.

After adding the dedicated meal-plan component, the rebuilt deployment returned
17 meal-plan entries with 17 attached thumbnails and completed the prefix-scoped
S3 startup check without warnings.

ChatGPT then showed the component stuck on "Preparing your meal plan…" from the
moment inline base64 thumbnails were added to the result. The widget script was
proven correct against a simulated MCP Apps host (handshake, tool-result render,
self-fetch recovery, and error path), so the fix removed the image blocks: the
redeployed `get_meal_plan_gallery` result for one day shrank from ~517 KB to
~17 KB with five private URLs in `_meta`, no URLs in structured content, the
`ui://tandoor/meal-plan-gallery-v9.html` binding, and the stale `-v8` URI still
resolving to the current HTML. ChatGPT then rendered the cards, but on the live
turn only after the chat was reloaded; v10 shortens the self-fetch delay, adds a
retry with timeout, names the loading phase in the status text, and reworks the
card design (day grouping, tinted tags, humanized times, collapsible notes).

On 2026-10-23 hosted ChatGPT rendered v10 correctly (3/3 thumbnails, private
URLs only in `_meta`, ~11 KB result) but the inline panel stayed blank until
the browser window was resized. Server-side logging ruled out the data path;
widget-side logging traced it to iframe sizing (see "Height reporting" in
`mcp.md`). v14 reports the height through `window.openai.notifyIntrinsicWidth`
as well as the MCP Apps notification; the recipe gallery (v8) carries the same
fix. ChatGPT caches the connector snapshot, so
after deploying a new widget URI use Settings → Apps & Connectors → Tandoor →
Actions → Refresh app before testing.

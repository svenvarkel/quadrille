# quadrille.dev: hosting, email and signup list — spec

*25 September 2026. Status: agreed direction, not built. Companion to [go-to-market.md](go-to-market.md).*

## Intent

Sven, 25.09: publish the landing page (`site/`) on quadrille.dev, which is already registered in AWS;
receive mail at the domain through Google Workspace; and collect email addresses of interested people
without gating the download ("kui me saaks koguda inimeste e-maili … sealt saaks mingit edasist
turundust mõelda"). Monetization is deferred; option B (approval gate for agent edits, Stablewood as
customer zero) is the preferred direction, and the list should help find its first users.

## Decisions

| Topic | Decision | Rejected | Why |
|---|---|---|---|
| Hosting | S3 (private) + CloudFront (OAC) + ACM + Route 53 | GitHub Pages, Cloudflare Pages, S3 website endpoint | Domain and DNS already live in AWS; OAC keeps the bucket private; S3 website endpoints have no HTTPS, and `.dev` is HSTS-preloaded |
| Infra as code | One CloudFormation template in `site/infra/` | Terraform, click-ops | A handful of resources, no state backend to run; click-ops is not reproducible |
| Inbound mail | Google Workspace, `quadrille.dev` as a user alias domain, `hello@` as an alias | Separate licence, SES inbound | Lands in Sven's existing inbox at no extra cost |
| Teams leads | `mailto:hello@quadrille.dev` on the page (already shipped) | Contact form | Leads are conversations; we want their problem in their words |
| Signup list | **MailerLite**, free plan | Buttondown, Kit, beehiiv, own Lambda + DynamoDB, CloudWatch Logs | See below |
| Download gate | **None.** The signup is voluntary | Email-for-download | `cargo`/`brew`/`uvx`/`npx` bypass the site anyway; agents do not fill in forms; it would dominate the HN thread |

### Signup provider comparison (pricing pages, 25.09.2026)

| | MailerLite | Buttondown | Kit | beehiiv |
|---|---|---|---|---|
| Free tier | 250 subscribers, 2,500 emails/mo, 3 forms | 100 subscribers | 10,000 subscribers | 2,500 subscribers |
| First paid step | from ~$12/mo | per active subscriber, add-ons $9–79/mo | $33/mo | $43/mo |
| Data location | **EU**, ISO 27001 | US | US | US |
| Custom field (regulated-data checkbox) | included | tagging is a $9/mo add-on | included | included |
| Sending from own domain | yes | yes, free | yes | yes, free |
| Branding on free plan | MailerLite logo | minimal | Kit logo | beehiiv logo |
| Fit | plain, EU, GDPR tooling | developer-friendly, Markdown, API | creator marketing | media publishing |

**MailerLite** wins on EU data residency for an Estonian controller (Wasabi OÜ), the free custom field,
and a free tier that covers the launch. **Buttondown** is the fallback if its developer tone and Markdown
archive start to matter more than EU residency and the $9 tagging add-on. The list is exportable as CSV
from either, so switching later is cheap.

Rejected outright: **CloudWatch Logs** (not a datastore: no per-record deletion, no unsubscribe, PII in
log retention); **own Lambda + DynamoDB** (≈40 lines, but double opt-in, unsubscribe, privacy notice and
sending reputation become ours to build and run).

## Acceptance criteria

1. `https://quadrille.dev/` serves `site/index.html` over HTTPS with a valid certificate; `http://` and
   `https://www.quadrille.dev/` redirect to it.
2. The S3 bucket is not publicly readable; only CloudFront can read it.
3. `site/deploy.sh` publishes the current `site/` and invalidates the cache in one command.
4. Mail to `hello@quadrille.dev` arrives in Sven's Google Workspace inbox; replies sent from that alias pass SPF, DKIM and DMARC alignment.
5. The page has a signup form: email plus an optional "I work with regulated data" checkbox. Submitting
   triggers a confirmation email (double opt-in); only confirmed addresses count as subscribers.
6. The checkbox value is stored as a MailerLite field and can be used to segment the list.
7. The page works with JavaScript disabled, and the form either submits as plain HTML or degrades to a visible `mailto:` fallback.
8. The page states what the address is used for and links to a privacy note (`site/privacy.html`).
9. No tracking scripts, pixels or third-party cookies on page load. MailerLite resources, if any, load only on form submit.

## Verification

| # | How |
|---|---|
| 1 | `curl -sI https://quadrille.dev/` → 200; `curl -sI http://quadrille.dev/` and `https://www.quadrille.dev/` → 301 to apex; `openssl s_client` shows ACM cert |
| 2 | `curl -sI https://<bucket>.s3.<region>.amazonaws.com/index.html` → 403 |
| 3 | Change a string in `site/index.html`, run `deploy.sh`, see it live within a minute |
| 4 | Send from an outside account (Gmail) and back; check `Authentication-Results` headers show `spf=pass dkim=pass dmarc=pass`; mail-tester.com ≥ 9/10 |
| 5–6 | Sign up two test addresses, one with the checkbox; confirm only one; MailerLite shows one active subscriber with the field set, the other pending |
| 7 | Load the page with JS disabled; submit the form |
| 8–9 | Read the page; DevTools Network tab on load shows only `quadrille.dev` and Google Fonts |

## Approach

### 1. AWS hosting — `site/infra/site.yaml` (CloudFormation, us-east-1)

us-east-1 because CloudFront only accepts ACM certificates from that region; keeping the whole stack
there avoids a cross-region certificate stack.

Resources:

- `AWS::S3::Bucket` — block all public access, SSE-S3, versioning on (cheap rollback).
- `AWS::CloudFront::OriginAccessControl` + bucket policy allowing only this distribution (`AWS:SourceArn`).
- `AWS::CertificateManager::Certificate` — `quadrille.dev`, SAN `www.quadrille.dev`, DNS validation
  against the existing hosted zone (`HostedZoneId` parameter).
- `AWS::CloudFront::Function` — redirect `www` → apex; rewrite `/foo/` → `/foo/index.html` for future pages.
- `AWS::CloudFront::Distribution` — `DefaultRootObject: index.html`, redirect-to-HTTPS, HTTP/2+3,
  managed `CachingOptimized` policy, a response-headers policy with HSTS
  (`max-age=63072000; includeSubDomains; preload`), `X-Content-Type-Options`, `Referrer-Policy`,
  and a CSP limited to self + `fonts.googleapis.com` / `fonts.gstatic.com` (+ the MailerLite
  submit origin once known). `PriceClass_100` (EU + NA) is enough.
- `AWS::Route53::RecordSet` — A and AAAA aliases for apex and `www`.

Parameters: `DomainName`, `HostedZoneId`. Outputs: bucket name, distribution ID.

Cost: hosted zone $0.50/mo (exists already); S3, CloudFront and ACM for a static page fit the free tier.

### 2. Deploy — `site/deploy.sh`

```sh
aws s3 sync site/ "s3://$BUCKET/" --delete --exclude 'infra/*' --exclude 'deploy.sh'
aws cloudfront create-invalidation --distribution-id "$DIST" --paths '/*'
```

`BUCKET` and `DIST` are read from the stack outputs (`aws cloudformation describe-stacks`), not
hardcoded. HTML gets `Cache-Control: max-age=300`, assets longer.

Rollback: S3 versioning restores the previous object; `aws cloudformation delete-stack` removes everything except the hosted zone (retain the bucket via `DeletionPolicy: Retain`).

### 3. Google Workspace inbound

Manual, in Admin console (Sven):

1. Account → Domains → Manage domains → **Add a domain** → `quadrille.dev` → *User alias domain*.
2. Copy the verification TXT record → Route 53.
3. Users → Sven → Add alternate email `hello@quadrille.dev`.
4. Apps → Gmail → Authenticate email → generate DKIM for `quadrille.dev` → copy the TXT record.
5. Gmail → Settings → Accounts → **Send mail as** `hello@quadrille.dev` so replies come from the alias.

Route 53 records (Claude can add these once AWS credentials work):

| Name | Type | Value |
|---|---|---|
| `quadrille.dev` | TXT | `google-site-verification=…` |
| `quadrille.dev` | MX | `1 smtp.google.com` |
| `quadrille.dev` | TXT | `v=spf1 include:_spf.google.com include:_spf.mlsend.com ~all` *(MailerLite include to be confirmed during setup)* |
| `google._domainkey.quadrille.dev` | TXT | DKIM key from Admin console |
| `_dmarc.quadrille.dev` | TXT | `v=DMARC1; p=none; rua=mailto:hello@quadrille.dev` → move to `p=quarantine` after two clean weeks |

Only one SPF record may exist; both senders go into the same one.

### 4. MailerLite signup

Setup (Sven creates the account; Claude does the rest):

1. Account under Wasabi OÜ, sender `hello@quadrille.dev`; authenticate the domain (MailerLite gives DKIM/SPF records → Route 53).
2. Group `quadrille-updates`; custom field `regulated_data` (checkbox / text `yes`).
3. Double opt-in on for the form; confirmation email in plain, short English.
4. Form: **embedded HTML form** if MailerLite offers a plain `<form action=…>` endpoint; otherwise their
   API through a tiny proxy is *not* worth it. Fall back to their hosted form page and link to it.

Page change (`site/index.html`), new `#updates` section above the footer:

> **Release notes, and early access to Quadrille for teams**
> [ email ] [ Subscribe ]
> ☐ I work with regulated data (finance, mortgage, health, public sector)
> *One email per release, no tracking. Unsubscribe from any email. [Privacy](privacy.html)*

- Styled with the existing tokens; no MailerLite CSS or JS on load (AC 9).
- Honeypot field against bots, in addition to whatever MailerLite provides.
- README gets one line: "Release notes: quadrille.dev#updates".

`site/privacy.html`: controller (Wasabi OÜ, address, `hello@quadrille.dev`), data (email + one field),
purpose (release notes, early access), processor (MailerLite, EU), retention (until unsubscribe),
rights (access, deletion), no analytics or cookies on the site.

### Order

1. AWS credentials renewed (`aws sso login`) → deploy the CloudFormation stack → `deploy.sh`. The page
   goes live **only after** `cargo install quadrille` works (see launch checklist in go-to-market.md).
2. Google Workspace alias domain + DNS records (can happen before the site is public).
3. MailerLite account → domain authentication → form → privacy page → deploy.

Steps 1 and 2 are independent. Step 3 needs step 2 for sender authentication.

## Open questions

- Does MailerLite's free plan allow a plain HTML form post without their JS? Verify when creating the form; it decides between AC 7 and the hosted-form fallback.
- Is double opt-in available on the free plan? Not stated on the pricing page.
- Should the signup also live in the README, or only link to the site? Proposed: link only.
- Repo host for the public link: the site points at `github.com/svenvarkel/quadrille`, while day-to-day git is on Bitbucket. Decide before launch.

## Out of scope

Analytics (add privacy-friendly, cookieless counting later if needed), a blog engine (blog posts can be
plain HTML pages in `site/` until there are more than a few), `schema.quadrille.dev`, and anything
paid.

# MDR-BUG-FLUX-00021 — mdrdp deploy signs the IDD driver catalogue without a timestamp, so the signature dies with the certificate

- **State:** Fixed
- **Priority:** Should
- **Severity:** Medium
- **Area:** deploy/driver-signing
- **Raised:** 2026-08-20T10:05:10Z
- **Discovery source:** Agent
- **Owner:** deltic:manual
- **Owner role:** verify
- **Owner run:** verify-20260913T073837Z-d0b3941a
- **Owner host:** flux
- **Owner branch:** task/bug-MDR-BUG-FLUX-00021-run-verify-20260913T073837Z-d0b3941a
- **Owner base:** 494c5d623f713288424d4ce6c8df321b96bfe5e4
- **Owner fingerprint:** sha256:ca99c34a288de3f98379021d403c8f3ebfbbc8ddf593039cfbf7fbc8399c9f82
- **Owner since:** 2026-09-13T07:38:37Z
- **Owner until:** 2026-09-13T09:38:37Z
- **Verify retry after:** -
- **Held branch:** -
- **Legacy fixed run:** -
- **Attempts:** fix=0, doubt=0, indeterminate=0
- **State history:** Open (2026-08-20T10:05:10Z, raised via `deltic bugs new` model=claude-opus-5@high) -> Fixed (2026-08-25T05:51:59Z, deltic:auto role=fix run=fix-20260825T054543Z-b566a704 branch=task/bug-MDR-BUG-FLUX-00021-run-fix-20260825T054543Z-b566a704 code=10b1135 gate=manual)

## Observation

src/deploy.rs:573 signs the driver catalogue with:

    signtool sign /fd SHA256 /sha1 $cert.Thumbprint (Join-Path $DriverDir 'mdrdp-idd.cat')

There is no /t. The documented manual runbook does timestamp -- tools/latency-spike/idd/README.md:103 uses

    signtool sign /fd SHA256 /sha1 $cert.Thumbprint /t http://timestamp.digicert.com

so the shipped deploy path lost a step the hand-written procedure has.

WHY IT MATTERS: an untimestamped Authenticode signature is only valid while the signing certificate is valid. A timestamped one stays valid after expiry, because the timestamp proves the signing happened while the cert was good. The self-signed cert deploy.rs creates is printed with its NotAfter at deploy time (src/deploy.rs:569) and is finite.

CONSEQUENCE: on the day that certificate expires, every host running a driver installed by 'mdrdp deploy' has a catalogue whose signature no longer verifies. A driver reinstall, a Windows update that re-verifies, or a PnP rebuild would then refuse the package. The failure arrives long after the change that caused it, on a date nobody is watching, and looks like the driver spontaneously breaking.

It is also silent today: nothing checks or reports the signature's validity window, so the health ladder would show a missing device rather than an expired signature.

FIX: add /t (or /tr with /td for RFC3161) to the signtool invocation in deploy.rs, matching the README. One line.

WORTH CHECKING WHILE THERE: Inf2Cat is invoked with /driver:$DriverDir, which catalogues the whole directory, while signtool signs exactly mdrdp-idd.cat (deploy.rs:571-573). That is correct for one driver package per directory and breaks quietly if a second INF is ever added alongside.

Found while scoping a possible second (audio) driver; unrelated to that decision and worth fixing regardless.

## Fix

<unfixed — raised only>

## Notes

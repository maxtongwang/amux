# Delegated grant passkeys

Passkeys add optional device user verification to pending delegated changes. `AMUX_DELEGATION_APPROVAL=passkey` selects the stronger approval proof; cite is the default. Other non-cite values fail closed. The parent PR owns the owner-only setting. Register before selecting passkey mode.

## Setup and operation

| Setting | Value |
| --- | --- |
| `AMUX_DELEGATION_PASSKEY_ORIGINS` | Comma-separated exact HTTPS origins. Default `https://amux.maxwangestates.com`. Add the MacBook's actual HTTPS tailnet hostname explicitly; no wildcards. |
| RP ID | Each configured origin's hostname; public DNS and tailnet hostnames require separate enrollments. |
| Enrollment | Settings → Delegated changes → Register passkey. Resident key preferred, client-device/hybrid hints, user verification required. Multiple credentials supported. |
| Initial trust | Owner bearer bootstrap, one time across all credentials, including revoked ones. WARN verdict `delegation_passkey_first_registration`, append-only delegation audit and leads feed. |
| Later enrollment | Owner bearer plus verified Remote origin, or an existing UV passkey at this RP. |
| Revocation | Owner bearer plus Remote origin or existing UV passkey. Dashboard requests a passkey. Revoked rows remain as history. |
| Approval | Approve invokes browser `get()` with required UV. Existing grant application atomically claims pending → applying and records the result. Typed lines cannot approve in passkey mode. |

## Proof and persistence

`webauthn-rs = 0.5.5` (MPL-2.0, Rust minimum 1.88; local toolchain 1.97.1) verifies challenge, signature, RP ID hash, exact origin and UV. No subdomain/any-port relaxation. Its serialized registration/authentication state lives only in SQLite, never in a client token. Its random challenge is bound to the exact grant code via the server-only ceremony row's purpose; a client cannot change the purpose of a stored nonce. Ceremonies expire after 120 seconds and are atomically consumed on finish, including invalid attempts. At most 64 unexpired ceremonies are stored.

Credential JSON contains the public key and credential ID, never the private key. Verification rechecks revocation and the latest counter inside the serialized store write. Nonzero counters must advance; zero-only synced passkeys remain usable. Credential updates and challenge consumption commit together. State survives process restart. A crash after consuming a proof but before applying leaves the grant pending; request a fresh challenge. Existing `applying` recovery belongs to the parent grant lifecycle; this change does not claim crash-atomic filesystem replay.

## Threat model and limits

Same-user workers can manufacture local process-origin verdicts or typed history. A cryptographically valid UV assertion requires the registered authenticator and its user verification; a forged process header cannot substitute for it. The protocol permits biometrics or a device PIN: it does not prove Touch ID specifically. Platform/hybrid hints express a preference, not an authenticator attestation policy.

Bootstrap deliberately trusts the owner bearer, so a same-user adversary holding it could enroll first. First-registration WARN/audit/leads visibility makes this exception conspicuous. Remote enrollment/revocation retains the parent's non-local origin trust boundary. Passkeys protect the API approval proof, not the SQLite file, process memory, owner token, or mode configuration against arbitrary code already running as the same OS user. OS isolation is still required for that stronger boundary (ai-config#136).

No enrollment is performed by this PR. An HTTPS browser and real Touch ID/Face ID ceremony are still needed for device acceptance testing. Parent PR #18's final cite/setting contract must be rebased before this draft becomes ready.

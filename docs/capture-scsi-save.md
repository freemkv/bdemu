# `scsi_save` — required vs. optional structure failures

`scsi_save` captures one SCSI metadata structure to `filename` and returns
`true` when a *required* structure failed for a reason that is NOT
"structure genuinely absent".

A bare `Err(_) => "n/a"` cannot tell an optional structure being missing
(fine for BCA/DCB) apart from a hard transport or drive failure that would
also break the required structures — so a capture missing required SCSI
metadata used to silently report clean. The rule is:

- ILLEGAL REQUEST / not-found sense -> `"n/a"` (structure genuinely absent, ok)
- transport failure / other hard error -> a distinct message, and for a
  required structure the failure is reported back so the caller can fold it
  into the fixture-completeness result.

# Protocol next (#311)

## Native compose

Native compose requests carry draft and selected-history revisions. `draft.get` reads the eligible editing snapshot and saved view anchor; `draft.save` updates them in one receipt-backed transaction. `draft.submit` preserves the frozen snapshot and uses the same operation identity for the accepted message. `draft.restore` reads an exact retained archive without replaying its submission.

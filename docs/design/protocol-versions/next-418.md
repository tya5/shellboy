# Thread branch selection revisions (#418)

`thread.branches` rows include `selection_revision`, the same stored revision
exposed as `history.window.revision`. When a divergence is present it includes
`parent_selection_revision`, the revision of the parent path used in that
comparison. Both revisions come from the branch row's read snapshot. Family
cursor scope and expiry are unchanged.

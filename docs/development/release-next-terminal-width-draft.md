# v0.7.3: terminal width configuration migration

This note describes the v0.7.3 candidate change, not a published release.

The manual `ui.ambiguous_width` setting introduced in v0.7.0 is removed. It let
users choose whether East Asian Ambiguous characters occupied one or two terminal
columns. Shellboy now measures terminal capabilities at startup without waiting
for replies before accepting input or drawing the first frame. A successful
measurement cached for that terminal supplies the initial baseline; unavailable
measurements remain unknown, with conservative fallback glyphs and widths.
Shellboy never persists a guessed measurement.

Before upgrading, remove only `ambiguous_width = ...` from the `[ui]` section of
your configuration, keeping all other settings. No replacement setting is needed.
The strict parser rejects the whole configuration if the obsolete key remains;
it does not silently ignore the key or choose a default. If startup reports this
unknown field, remove it and restart with the same configuration path.

# Protocol next (#316)

Thread artifact metadata uses `artifact.add`, `artifact.update`, `artifact.list`,
`artifact.get`, and `artifact.remove`. The owner stores opaque URI, title, and
optional MIME strings; updates append versions. List pages use descending update
time and ascending ID, and `artifact.get` can read an exact version or page
through version history. See [Thread artifacts](../thread-artifacts.md) for request
limits, cursor scope, and lifecycle behavior.

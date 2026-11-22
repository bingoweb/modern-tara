package web

import "embed"

// dist is build output git does not track.
//
//go:embed dist
var FS embed.FS

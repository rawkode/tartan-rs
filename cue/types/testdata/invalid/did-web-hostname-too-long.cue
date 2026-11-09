package test

import (
	"strings"

	"tartan.rawkode.academy/types"
)

d: types.#DID & "did:web:\(strings.Repeat("a", 60)).\(strings.Repeat("a", 60)).\(strings.Repeat("a", 60)).\(strings.Repeat("a", 60)).\(strings.Repeat("a", 60)).com"

package settings

import "tartan.dev/ext/types"

#Settings: close({
	// Issue fields, by name. Each knows its name, so another extension's config
	// can reference it. These are the field types Issues stores and validates.
	fields?: close({[Name=string & =~"^[a-z][a-z0-9_]{0,25}$"]: types.#Field & {
		name: Name
		type: "text" | "number" | "select" | "multiselect" | "monetary" | "date" | "boolean"
	}})
})

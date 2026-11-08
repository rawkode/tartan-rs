// Package issues is the settings schema of Issues (`rawkode.issues`).
//
// A repository installs Issues by unifying its entry with #Extension:
//
//	extensions: "rawkode.issues": issues.#Extension & {settings: properties: {
//		status: types.#Select & {options: ["todo", "doing", "done"]}
//	}}
//
// #Extension fixes the extension's id and the version this module packages, so
// the module version a repository depends on is the version it installs.
package issues

import "tartan.dev/types"

// An installation of Issues on a repository.
#Extension: {
	id:       "rawkode.issues"
	version:  "0.6.0"
	settings: #Settings
}

// Issues' settings.
#Settings: {
	// The properties every issue of the repository may have, by name. Each
	// property records its own name, so another extension's configuration can
	// refer to the property itself (`settings.properties.status`) and CUE
	// checks the reference.
	properties: [Name=types.#Name]: types.#Property & {name: Name}
}

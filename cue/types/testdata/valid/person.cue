package test

import "tartan.rawkode.academy/types"

p: types.#Person & {label: "Owner", default: "did:plc:abcdefghijklmnopqrstuvwx"}
web: types.#Person & {default: "did:web:rawkode.dev"}
none: types.#Person

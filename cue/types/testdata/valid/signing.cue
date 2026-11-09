package test

import "tartan.rawkode.academy/types"

main: types.#Signing & {require: [{refs: ["refs/heads/main"]}]}

rules: types.#Signing & {require: [{
	refs: ["refs/heads/main", "refs/heads/release-*", "refs/tags/**"]
	signers: ["did:plc:abcdefghijklmnopqrstuvwx", "did:web:rawkode.dev"]
}, {
	refs: ["refs/heads/*/stable"]
	signers: ["did:web:localhost%3A9400"]
}]}

nothing: types.#Signing & {require: []}

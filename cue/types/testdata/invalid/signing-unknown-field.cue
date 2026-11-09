package test

import "tartan.rawkode.academy/types"

s: types.#Signing & {require: [{refs: ["refs/heads/main"], allow: ["did:web:rawkode.dev"]}]}

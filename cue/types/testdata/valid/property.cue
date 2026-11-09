package test

import "tartan.rawkode.academy/types"

p: types.#Property & {kind: "money", currency: "GBP"}
q: types.#Property & {kind: "select", options: ["a"], name: "stage"}
r: types.#Property & {kind: "longtext"}
s: [N=string]: types.#Property & {name: N}
s: {status: {kind: "select", options: ["a", "b"]}, due: {kind: "date"}}

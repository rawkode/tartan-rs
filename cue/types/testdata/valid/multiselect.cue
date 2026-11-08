package test

import "tartan.dev/types"

p: types.#MultiSelect & {options: ["bug", "docs"], labels: bug: "Bug", default: ["docs"]}

package test

import "tartan.dev/types"

p: types.#Select & {options: ["todo", "doing", "done"], labels: {todo: "To do"}, default: "todo"}

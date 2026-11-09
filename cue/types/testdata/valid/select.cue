package test

import "tartan.rawkode.academy/types"

p: types.#Select & {options: ["todo", "doing", "done"], labels: {todo: "To do"}, default: "todo"}

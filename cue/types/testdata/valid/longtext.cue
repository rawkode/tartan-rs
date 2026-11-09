package test

import "tartan.rawkode.academy/types"

p: types.#LongText & {label: "Notes", maxLength: 5000, default: "# Notes\n\n- one"}

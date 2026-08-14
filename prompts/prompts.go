package prompts

import _ "embed"

//go:embed memory.txt
var Memory string

//go:embed retrieval.txt
var Retrieval string

//go:embed summary.txt
var Summary string

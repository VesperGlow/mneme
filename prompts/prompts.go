package prompts

import _ "embed"

//go:embed system.txt
var System string

//go:embed persona.txt
var Persona string

//go:embed memory.txt
var Memory string

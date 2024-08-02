package main

import (
	"errors"
	"log"
	"os"

	"github.com/urfave/cli/v2"
)

func main() {
	app := &cli.App{
		Name:  "vulcan",
		Usage: "manage your personal wiki",
		Commands: []*cli.Command{
			{
				Name:  "edit",
				Usage: "edit a wiki page",
				Action: func(c *cli.Context) error {
					// TODO implement me
					return errors.New("not implemented")
				},
			},
			{
				Name:  "shell",
				Usage: "run an interactive shell in the wiki repo",
				Action: func(c *cli.Context) error {
					// TODO implement me
					return errors.New("not implemented")
				},
			},
			{
				Name:    "quick",
				Aliases: []string{"q"},
				Usage:   "Edit quicknotes",
				Action: func(c *cli.Context) error {
					// TODO implement me
					return errors.New("not implemented")
				},
			},
			{
				Name:  "git",
				Usage: "run a git command in the wiki repo",
				Action: func(c *cli.Context) error {
					// TODO implement me
					return errors.New("not implemented")
				},
			},
			{
				Name:  "ls",
				Usage: "list all wiki pages matching optional pattern",
				Action: func(c *cli.Context) error {
					// TODO implement me
					return errors.New("not implemented")
				},
			},
			{
				Name:  "sql",
				Usage: "run an SQL query on the wiki database",
				Action: func(c *cli.Context) error {
					// TODO implement me
					// implement using a virtual table
					// add row-level ACLs later? https://sqlite.org/forum/info/2e4b58ca45b0de363d3d652fc7ebcfed951daa8b0e585187df92b37a229d5dc5
					return errors.New("not implemented")
				},
			},
			{
				Name:  "log",
				Usage: "edit logbook",
				Action: func(c *cli.Context) error {
					// TODO implement me
					return errors.New("not implemented")
				},
			},
			{
				Name:  "mv",
				Usage: "move a wiki page and update all links",
				Action: func(c *cli.Context) error {
					// TODO implement me
					return errors.New("not implemented")
				},
			},
			{
				Name:  "rm",
				Usage: "remove a wiki page and update all links",
				Action: func(c *cli.Context) error {
					// TODO implement me
					return errors.New("not implemented")
				},
			},
			{
				Name:  "ai",
				Usage: "ai tools for your wiki",
				Subcommands: []*cli.Command{
					{
						Name:  "summarize",
						Usage: "summarize wiki pages matching pattern",
						Action: func(c *cli.Context) error {
							// TODO implement me
							return errors.New("not implemented")
						},
					},
					{
						Name:  "ask",
						Usage: "ask a question to your wiki, using pages specified by pattern or search term as context",
						Action: func(c *cli.Context) error {
							// TODO implement me
							return errors.New("not implemented")
						},
					},
				},
			},
		},
	}

	if err := app.Run(os.Args); err != nil {
		log.Fatal(err)
	}
}

// Live read-only test of the generated Go SDK against the real Cadenya API.
// Requires CADENYA_API_KEY and CADENYA_WORKSPACE_ID in the environment
// (source .env.development). Never prints secrets; IDs are truncated.
//
// Usage: source .env.development && go run ./e2e/live-go
package main

import (
	"context"
	"errors"
	"fmt"
	"os"

	sdk "go.cadenya.com/cadenya-go"
)

func short(id string) string {
	if len(id) > 12 {
		return id[:12] + "…"
	}
	return id
}

func fatal(step string, err error) {
	fmt.Printf("%-20s FAIL %v\n", step, err)
	os.Exit(1)
}

func main() {
	if os.Getenv("CADENYA_API_KEY") == "" || os.Getenv("CADENYA_WORKSPACE_ID") == "" {
		fmt.Println("missing CADENYA_API_KEY / CADENYA_WORKSPACE_ID")
		os.Exit(1)
	}

	// Key AND workspace come from env — no per-call workspaceId below
	// exercises the client-defaults feature live.
	client, err := sdk.NewClient()
	if err != nil {
		fatal("NewClient", err)
	}
	ctx := context.Background()

	// 1. Credentials check.
	// account.info carries secret material (webhook HMAC secret) — never print it.
	account, err := client.Accounts().Retrieve(ctx)
	if err != nil {
		fatal("accounts.retrieve", err)
	}
	fmt.Printf("accounts.retrieve   ok  info present: %v\n", account.Info != nil)

	// 2. Workspaces list (pagination envelope against real data).
	workspaces, err := client.Workspaces().List(ctx, &sdk.WorkspaceListParams{Limit: sdk.Int32(2)})
	if err != nil {
		fatal("workspaces.list", err)
	}
	fmt.Printf("workspaces.list     ok  %d item(s), hasNextPage=%v\n", len(workspaces.Items), workspaces.HasNextPage())

	// 3. Agents in the provided workspace.
	agents, err := client.Agents().List(ctx, &sdk.AgentListParams{Limit: sdk.Int32(3)})
	if err != nil {
		fatal("agents.list", err)
	}
	ids := ""
	for _, a := range agents.Items {
		if a.Metadata != nil {
			if ids != "" {
				ids += ", "
			}
			ids += short(a.Metadata.ID)
		}
	}
	fmt.Printf("agents.list         ok  %s\n", ids)

	// 4. Objectives + pagination across real pages (capped at 5).
	page, err := client.Objectives().List(ctx, &sdk.ObjectiveListParams{Limit: sdk.Int32(2)})
	if err != nil {
		fatal("objectives.list", err)
	}
	seen := 0
	for page != nil && seen < 5 {
		seen += len(page.Items)
		if !page.HasNextPage() {
			break
		}
		page, err = page.GetNextPage(ctx)
		if err != nil {
			fatal("objectives.list page", err)
		}
	}
	fmt.Printf("objectives.list     ok  %d across pages\n", seen)

	// 5. Models catalog.
	models, err := client.Models().List(ctx, &sdk.ModelListParams{Limit: sdk.Int32(3)})
	var apiErr *sdk.APIError
	if errors.As(err, &apiErr) {
		fmt.Printf("models.list         skip APIError %d: %s\n", apiErr.StatusCode, apiErr.Message)
	} else if err != nil {
		fatal("models.list", err)
	} else {
		fmt.Printf("models.list         ok  %d item(s)\n", len(models.Items))
	}

	// 6. Error mapping against the real server.
	_, err = client.Objectives().Retrieve(ctx, "obj_does_not_exist", nil)
	if err == nil {
		fmt.Println("error mapping       FAIL (expected an APIError)")
		os.Exit(1)
	}
	if !errors.As(err, &apiErr) {
		fatal("error mapping", err)
	}
	fmt.Printf("error mapping       ok  status=%d code=%d\n", apiErr.StatusCode, apiErr.Code)

	fmt.Println("\nlive API checks passed (go)")
}

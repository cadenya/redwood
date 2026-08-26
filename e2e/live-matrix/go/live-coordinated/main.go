// Serialized, account/shared-state tail for the Go live matrix.
//
// DO NOT run concurrently with another SDK lane. Rotating the global API key
// invalidates the old global token. The ambient credential remains the
// recovery/controller credential and is never rewritten. The runner retrieves
// and chains the current/new global token only in memory and never prints it.
//
// AI provider lifecycle requires GO_LIVE_PROVIDER_API_KEY; its value is never
// printed or persisted. Workspace member lifecycle requires
// GO_LIVE_MEMBER_PROFILE_ID and must name a profile not already in the new
// workspace. Objective approval/deny/bare-content probes require suitable
// pending tool-call fixtures and remain dependency-blocked otherwise.
package main

import (
	"context"
	"encoding/json"
	"errors"
	"fmt"
	"os"
	"path/filepath"
	"time"

	sdk "go.cadenya.com/cadenya-go"
)

type result struct {
	Status   string `json:"status"`
	Evidence string `json:"evidence"`
}
type report struct {
	SchemaVersion int               `json:"schemaVersion"`
	SDK           string            `json:"sdk"`
	ExecutedAt    string            `json:"executedAt"`
	Operations    map[string]result `json:"operations"`
}
type wave struct {
	client      *sdk.Client
	results     map[string]result
	run         string
	workspaceID string
}

func main() {
	if os.Getenv("GO_LIVE_COORDINATED") != "1" {
		fmt.Println("refusing coordinated Go tail without GO_LIVE_COORDINATED=1")
		return
	}
	if os.Getenv("CADENYA_API_KEY") == "" || os.Getenv("CADENYA_WORKSPACE_ID") == "" {
		fatal(errors.New("CADENYA_API_KEY and CADENYA_WORKSPACE_ID are required"))
	}
	client, err := sdk.NewClient()
	if err != nil {
		fatal(err)
	}
	w := &wave{client: client, results: readResults(), run: fmt.Sprintf("go-coordinated-%d", time.Now().UnixMilli()), workspaceID: os.Getenv("CADENYA_WORKSPACE_ID")}
	if os.Getenv("GO_LIVE_COORDINATED_ADMIN_READS_ONLY") == "1" {
		w.adminReads()
		writeResults(w.results)
		return
	}
	w.rotateAccountCredentials()
	w.adminReads()
	w.workspaceLifecycle()
	w.providerLifecycle()
	w.modelLifecycle()
	w.blockObjectiveTail()
	writeResults(w.results)
	counts := map[string]int{}
	for _, v := range w.results {
		counts[v.Status]++
	}
	fmt.Printf("go coordinated tail: %d completed, %d failed, %d blocked (%d cumulative)\n", counts["completed"], counts["failed"], counts["blocked"], len(w.results))
}
func fatal(err error) { fmt.Fprintln(os.Stderr, "go coordinated tail failed:", err); os.Exit(1) }
func readResults() map[string]result {
	raw, err := os.ReadFile(filepath.Join("..", "..", "results-go.json"))
	if err != nil {
		return map[string]result{}
	}
	var r report
	if json.Unmarshal(raw, &r) != nil {
		return map[string]result{}
	}
	return r.Operations
}
func writeResults(ops map[string]result) {
	raw, err := json.MarshalIndent(report{1, "go", time.Now().UTC().Format(time.RFC3339), ops}, "", "  ")
	if err != nil {
		fatal(err)
	}
	raw = append(raw, '\n')
	if os.WriteFile(filepath.Join("..", "..", "results-go.json"), raw, 0o644) != nil {
		fatal(errors.New("write results"))
	}
}
func (w *wave) probe(id, evidence string, fn func(context.Context) error) bool {
	ctx, cancel := context.WithTimeout(context.Background(), 45*time.Second)
	defer cancel()
	err := fn(ctx)
	if err == nil {
		w.results[id] = result{"completed", evidence}
		return true
	}
	var a *sdk.APIError
	if errors.As(err, &a) {
		status := "failed"
		if a.StatusCode == 403 {
			status = "blocked"
		}
		w.results[id] = result{status, fmt.Sprintf("Real API returned HTTP %d / API code %d; no body, credential, or identifier persisted.", a.StatusCode, a.Code)}
	} else if unmarshalErr := (*json.UnmarshalTypeError)(nil); errors.As(err, &unmarshalErr) {
		w.results[id] = result{"failed", fmt.Sprintf("Real API returned JSON value kind %q at response field %q, incompatible with generated Go type %q; no body, credential, or identifier persisted.", unmarshalErr.Value, unmarshalErr.Field, unmarshalErr.Type.String())}
	} else if syntaxErr := (*json.SyntaxError)(nil); errors.As(err, &syntaxErr) {
		w.results[id] = result{"failed", "Real API returned syntactically invalid JSON for the generated Go response type; no body, credential, or identifier persisted."}
	} else {
		inner := errors.Unwrap(err)
		if inner != nil {
			w.results[id] = result{"failed", fmt.Sprintf("Generated Go SDK returned %T wrapping %T; no body, credential, or identifier persisted.", err, inner)}
		} else {
			w.results[id] = result{"failed", fmt.Sprintf("Generated Go SDK returned %T; no body, credential, or identifier persisted.", err)}
		}
	}
	return false
}
func (w *wave) blocked(ids []string, why string) {
	for _, id := range ids {
		w.results[id] = result{"blocked", why}
	}
}

func (w *wave) rotateAccountCredentials() {
	controller := w.client
	global, e := controller.APIKeys().RetrieveGlobal(context.Background())
	if e != nil || global == nil || global.Spec == nil || global.Spec.Token == nil || *global.Spec.Token == "" {
		w.blocked([]string{"GlobalAPIKeyService_DisableGlobalAPIKey", "GlobalAPIKeyService_EnableGlobalAPIKey", "GlobalAPIKeyService_RotateGlobalAPIKey"}, "Controller credential could not retrieve the current global token; no auth mutation was sent.")
		return
	}
	elevated, err := sdk.NewClient(sdk.WithAPIKey(*global.Spec.Token), sdk.WithWorkspaceID(w.workspaceID))
	if err != nil {
		fatal(err)
	}
	w.client = elevated
	w.probe("AccountService_RotateChallengeToken", "Rotated account challenge token and decoded nonblank response; value not printed/persisted.", func(ctx context.Context) error {
		v, e := w.client.Accounts().RotateChallengeToken(ctx)
		if e != nil {
			return e
		}
		if v == nil || v.ChallengeToken == nil || *v.ChallengeToken == "" {
			return errors.New("blank challenge token")
		}
		return nil
	})
	w.probe("AccountService_RotateWebhookSigningKey", "Rotated webhook signing key and decoded nonblank response; value not printed/persisted.", func(ctx context.Context) error {
		v, e := w.client.Accounts().RotateWebhookSigningKey(ctx)
		if e != nil {
			return e
		}
		if v == nil || v.WebhookEventsHMACSecret == nil || *v.WebhookEventsHMACSecret == "" {
			return errors.New("blank webhook key")
		}
		return nil
	})
	// The ambient credential may or may not be the managed global key. When it
	// is the same token, rotating would invalidate the only recovery credential
	// unless a durable replacement destination is prepared first.
	if os.Getenv("CADENYA_API_KEY") == *global.Spec.Token {
		w.blocked([]string{"GlobalAPIKeyService_RotateGlobalAPIKey", "GlobalAPIKeyService_DisableGlobalAPIKey", "GlobalAPIKeyService_EnableGlobalAPIKey"}, "Ambient credential is the managed global key; coordinated runner refuses rotation/disable without durable replacement and independent recovery.")
		return
	}
	var token string
	if !w.probe("GlobalAPIKeyService_RotateGlobalAPIKey", "Rotated global API key via controller and decoded a nonblank token; token kept only in memory.", func(ctx context.Context) error {
		v, e := controller.APIKeys().RotateGlobal(ctx)
		if e != nil {
			return e
		}
		if v != nil && v.Spec != nil && v.Spec.Token != nil {
			token = *v.Spec.Token
		}
		if token == "" {
			fresh, getErr := controller.APIKeys().RetrieveGlobal(ctx)
			if getErr != nil {
				return getErr
			}
			if fresh == nil || fresh.Spec == nil || fresh.Spec.Token == nil {
				return errors.New("blank global token")
			}
			token = *fresh.Spec.Token
		}
		return nil
	}) {
		w.blocked([]string{"GlobalAPIKeyService_DisableGlobalAPIKey", "GlobalAPIKeyService_EnableGlobalAPIKey"}, "Global rotation failed; continuing could invalidate all SDK lanes.")
		return
	}
	client, err := sdk.NewClient(sdk.WithAPIKey(token), sdk.WithWorkspaceID(w.workspaceID))
	if err != nil {
		fatal(err)
	}
	w.client = client
	os.Setenv("CADENYA_API_KEY", token)
	w.probe("GlobalAPIKeyService_DisableGlobalAPIKey", "Disabled rotated global key via recovery/controller credential.", func(ctx context.Context) error { _, e := controller.APIKeys().DisableGlobal(ctx); return e })
	enable := func(ctx context.Context) error { _, e := controller.APIKeys().EnableGlobal(ctx); return e }
	if !w.probe("GlobalAPIKeyService_EnableGlobalAPIKey", "Re-enabled rotated global key via recovery/controller credential.", enable) {
		// Restoration gets two additional best-effort attempts before any other
		// operation uses the rotated global credential.
		if !w.probe("GlobalAPIKeyService_EnableGlobalAPIKey", "Re-enabled rotated global key via recovery/controller credential after a cleanup retry.", enable) {
			w.probe("GlobalAPIKeyService_EnableGlobalAPIKey", "Re-enabled rotated global key via recovery/controller credential after cleanup retries.", enable)
		}
	}
}

func (w *wave) adminReads() {
	w.probe("WorkspaceAdminService_ListProfiles", "Listed account profiles with the elevated current-run credential and decoded the page without persisting profile data.", func(ctx context.Context) error {
		page, err := w.client.WorkspaceAdmin().ListProfiles(ctx, &sdk.WorkspaceAdminListProfilesParams{Limit: sdk.Int32(20)})
		if err == nil && page == nil {
			return errors.New("profiles page absent")
		}
		return err
	})
	w.probe("WorkspaceAdminService_ListAccountWorkspaces", "Listed account workspaces with the elevated current-run credential and decoded the page without persisting workspace data.", func(ctx context.Context) error {
		page, err := w.client.WorkspaceAdmin().ListAccount(ctx, &sdk.WorkspaceAdminListAccountParams{Limit: sdk.Int32(20)})
		if err == nil && page == nil {
			return errors.New("workspace page absent")
		}
		return err
	})
	w.probe("WorkspaceAdminService_GetWorkspace", "Retrieved the configured workspace with the elevated current-run credential and decoded metadata.", func(ctx context.Context) error {
		value, err := w.client.WorkspaceAdmin().Retrieve(ctx, &sdk.WorkspaceAdminRetrieveParams{WorkspaceID: sdk.String(w.workspaceID)})
		if err == nil && (value == nil || value.Metadata == nil) {
			return errors.New("workspace metadata absent")
		}
		return err
	})
	w.probe("WorkspaceAdminService_ListWorkspaceMembers", "Listed configured-workspace members with the elevated current-run credential and decoded the page without persisting member data.", func(ctx context.Context) error {
		page, err := w.client.WorkspaceAdmin().ListMembers(ctx, &sdk.WorkspaceAdminListMembersParams{WorkspaceID: sdk.String(w.workspaceID), Limit: sdk.Int32(20)})
		if err == nil && page == nil {
			return errors.New("workspace-member page absent")
		}
		return err
	})
}

func (w *wave) workspaceLifecycle() {
	var id string
	if !w.probe("WorkspaceAdminService_CreateWorkspace", "Created uniquely named disposable workspace and decoded metadata.", func(ctx context.Context) error {
		v, e := w.client.WorkspaceAdmin().Create(ctx, &sdk.WorkspaceAdminCreateParams{Metadata: &sdk.CreateAccountResourceMetadata{Name: w.run + "-workspace", Labels: map[string]string{"redwood-live": "go"}}, Spec: &sdk.WorkspaceSpec{Description: sdk.String("Go coordinated tail")}})
		if e != nil {
			return e
		}
		if v == nil || v.Metadata == nil {
			return errors.New("workspace metadata absent")
		}
		id = v.Metadata.ID
		return nil
	}) {
		w.blocked([]string{"WorkspaceAdminService_GetWorkspace", "WorkspaceAdminService_UpdateWorkspace", "WorkspaceAdminService_AddWorkspaceMember", "WorkspaceAdminService_RemoveWorkspaceMember", "WorkspaceAdminService_ArchiveWorkspace"}, "Disposable workspace creation failed; dependent operations were not sent.")
		return
	}
	w.probe("WorkspaceAdminService_GetWorkspace", "Retrieved disposable workspace and decoded metadata.", func(ctx context.Context) error {
		v, e := w.client.WorkspaceAdmin().Retrieve(ctx, &sdk.WorkspaceAdminRetrieveParams{WorkspaceID: sdk.String(id)})
		if e != nil {
			return e
		}
		if v == nil || v.Metadata == nil {
			return errors.New("workspace metadata absent")
		}
		return nil
	})
	w.probe("WorkspaceAdminService_UpdateWorkspace", "Updated disposable workspace description and decoded response.", func(ctx context.Context) error {
		_, e := w.client.WorkspaceAdmin().Update(ctx, &sdk.WorkspaceAdminUpdateParams{WorkspaceID: sdk.String(id), Spec: &sdk.WorkspaceSpec{Description: sdk.String("Go coordinated tail updated")}, UpdateMask: sdk.String("spec.description")})
		return e
	})
	profile := os.Getenv("GO_LIVE_MEMBER_PROFILE_ID")
	if profile == "" {
		w.blocked([]string{"WorkspaceAdminService_AddWorkspaceMember", "WorkspaceAdminService_RemoveWorkspaceMember"}, "GO_LIVE_MEMBER_PROFILE_ID is unset; no member mutation was sent.")
	} else if w.probe("WorkspaceAdminService_AddWorkspaceMember", "Added explicitly supplied test profile to disposable workspace and decoded response.", func(ctx context.Context) error {
		_, e := w.client.WorkspaceAdmin().AddMember(ctx, &sdk.WorkspaceAdminAddMemberParams{WorkspaceID: sdk.String(id), ProfileID: sdk.String(profile)})
		return e
	}) {
		w.probe("WorkspaceAdminService_RemoveWorkspaceMember", "Removed test profile from disposable workspace.", func(ctx context.Context) error {
			return w.client.WorkspaceAdmin().RemoveMember(ctx, profile, &sdk.WorkspaceAdminRemoveMemberParams{WorkspaceID: sdk.String(id)})
		})
	}
	archive := func(ctx context.Context) error {
		return w.client.WorkspaceAdmin().Archive(ctx, &sdk.WorkspaceAdminArchiveParams{WorkspaceID: sdk.String(id)})
	}
	if !w.probe("WorkspaceAdminService_ArchiveWorkspace", "Archived disposable workspace.", archive) {
		w.probe("WorkspaceAdminService_ArchiveWorkspace", "Archived disposable workspace after a cleanup retry.", archive)
	}
}

func (w *wave) providerLifecycle() {
	secret := os.Getenv("GO_LIVE_PROVIDER_API_KEY")
	if secret == "" {
		w.blocked([]string{"AIProviderKeyService_CreateAIProviderKey", "AIProviderKeyService_UpdateAIProviderKey", "AIProviderKeyService_DeleteAIProviderKey"}, "GO_LIVE_PROVIDER_API_KEY is unset; no provider credential mutation was sent.")
		return
	}
	credential := sdk.NewAIProviderCredentialAPIKey(sdk.AIProviderCredential_APIKey{APIKey: &sdk.CredentialAPIKey{APIKey: sdk.String(secret)}})
	var id string
	if !w.probe("AIProviderKeyService_CreateAIProviderKey", "Created unique provider key and decoded metadata; credential not printed/persisted.", func(ctx context.Context) error {
		v, e := w.client.AIProviderKeys().Create(ctx, &sdk.AIProviderKeyCreateParams{Metadata: &sdk.CreateResourceMetadata{Name: w.run + "-provider", Labels: map[string]string{"redwood-live": "go"}}, Spec: &sdk.AIProviderKeySpec{Provider: sdkPtr(sdk.AIProviderKeySpecProviderAIProviderOpenAI), Credentials: &credential}})
		if e != nil {
			return e
		}
		if v == nil || v.Metadata == nil {
			return errors.New("provider metadata absent")
		}
		id = v.Metadata.ID
		return nil
	}) {
		w.blocked([]string{"AIProviderKeyService_UpdateAIProviderKey", "AIProviderKeyService_DeleteAIProviderKey"}, "Owned provider-key fixture unavailable.")
		return
	}
	w.probe("AIProviderKeyService_UpdateAIProviderKey", "Updated owned provider-key name; credential not printed/persisted.", func(ctx context.Context) error {
		_, e := w.client.AIProviderKeys().Update(ctx, id, &sdk.AIProviderKeyUpdateParams{Metadata: &sdk.UpdateResourceMetadata{Name: w.run + "-provider-updated"}, UpdateMask: sdk.String("metadata.name")})
		return e
	})
	remove := func(ctx context.Context) error { return w.client.AIProviderKeys().Delete(ctx, id, nil) }
	if !w.probe("AIProviderKeyService_DeleteAIProviderKey", "Deleted owned provider-key fixture.", remove) {
		w.probe("AIProviderKeyService_DeleteAIProviderKey", "Deleted owned provider-key fixture after a cleanup retry.", remove)
	}
}
func sdkPtr[T any](v T) *T { return &v }

func (w *wave) modelLifecycle() {
	page, err := w.client.Models().List(context.Background(), &sdk.ModelListParams{Limit: sdk.Int32(50)})
	if err != nil || page == nil || len(page.Items) < 2 {
		w.blocked([]string{"ModelService_DisableModel", "ModelService_EnableModel", "ModelService_SwapModelOnVariations"}, "At least two list-derived models are required; no model mutation was sent.")
		return
	}
	var enabled *sdk.Model
	for i := range page.Items {
		if page.Items[i].Metadata != nil && page.Items[i].State == sdk.ModelStateStateEnabled {
			enabled = &page.Items[i]
			break
		}
	}
	if enabled == nil {
		w.blocked([]string{"ModelService_DisableModel", "ModelService_EnableModel", "ModelService_SwapModelOnVariations"}, "No enabled model fixture; no model mutation was sent.")
		return
	}
	id := enabled.Metadata.ID
	w.probe("ModelService_DisableModel", "Disabled list-derived model and decoded response.", func(ctx context.Context) error { _, e := w.client.Models().Disable(ctx, id, nil); return e })
	enable := func(ctx context.Context) error { _, e := w.client.Models().Enable(ctx, id, nil); return e }
	if !w.probe("ModelService_EnableModel", "Re-enabled list-derived model and decoded response.", enable) {
		if !w.probe("ModelService_EnableModel", "Re-enabled list-derived model after a cleanup retry.", enable) {
			w.probe("ModelService_EnableModel", "Re-enabled list-derived model after cleanup retries.", enable)
		}
	}
	// A self-swap is intentionally side-effect neutral but still exercises the
	// whole request/response path; the backend accepts idempotent swaps.
	w.probe("ModelService_SwapModelOnVariations", "Submitted idempotent self-swap for a list-derived model and decoded empty response.", func(ctx context.Context) error {
		return w.client.Models().SwapOnVariations(ctx, &sdk.ModelSwapOnVariationsParams{ModelSwaps: []sdk.SwapModelOnVariationsRequest_ModelSwap{{CurrentModelID: sdk.String(id), NextModelID: sdk.String(id)}}})
	})
}

func (w *wave) blockObjectiveTail() {
	for _, id := range []string{"ObjectiveService_CreateObjective", "ObjectiveService_CreateObjectiveFeedback", "ObjectiveService_ApproveToolCall", "ObjectiveService_DenyToolCall", "ObjectiveService_SetToolCallContent", "ObjectiveService_CancelObjective", "ObjectiveService_CompactObjective", "ObjectiveService_ContinueObjective"} {
		if current, ok := w.results[id]; ok && current.Status == "completed" {
			continue
		}
		w.results[id] = result{"blocked", "Requires purpose-built objective/tool-call state; root's serialized final flow supplies and coordinates these fixtures."}
	}
}

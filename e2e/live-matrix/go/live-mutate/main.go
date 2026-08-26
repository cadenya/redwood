// Reproducible mutation wave for the generated Go SDK. Every resource is
// uniquely named and owned by this run. Cleanup operations are themselves
// live-tested API operations and execute even after intermediate failures.
// Account-global credential rotations and workspace administration are left
// to the serialized cross-SDK final wave because they can invalidate peers.
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
	client  *sdk.Client
	results map[string]result
	run     string
}

func main() {
	if os.Getenv("CADENYA_API_KEY") == "" || os.Getenv("CADENYA_WORKSPACE_ID") == "" {
		fatal(errors.New("CADENYA_API_KEY and CADENYA_WORKSPACE_ID are required"))
	}
	client, err := sdk.NewClient()
	if err != nil {
		fatal(err)
	}
	w := &wave{client: client, results: readResults(), run: fmt.Sprintf("go-live-%d", time.Now().UnixMilli())}
	w.runResources()
	w.recordDeferredOperations()
	writeResults(w.results)
	counts := map[string]int{}
	for _, value := range w.results {
		counts[value.Status]++
	}
	fmt.Printf("go cumulative live matrix: %d completed, %d failed, %d blocked (%d recorded)\n", counts["completed"], counts["failed"], counts["blocked"], len(w.results))
}

// recordDeferredOperations makes a fresh read+mutation run account for the
// entire 142-operation contract without borrowing historical completions.
// The specialized runner overwrites its objective entries later; root's
// serialized coordinator overwrites shared account/admin/provider/model ones.
func (w *wave) recordDeferredOperations() {
	shared := []string{
		"AccountService_RotateChallengeToken", "AccountService_RotateWebhookSigningKey",
		"GlobalAPIKeyService_DisableGlobalAPIKey", "GlobalAPIKeyService_EnableGlobalAPIKey", "GlobalAPIKeyService_RotateGlobalAPIKey",
		"WorkspaceAdminService_CreateWorkspace", "WorkspaceAdminService_ArchiveWorkspace", "WorkspaceAdminService_UpdateWorkspace", "WorkspaceAdminService_AddWorkspaceMember", "WorkspaceAdminService_RemoveWorkspaceMember",
		"AIProviderKeyService_CreateAIProviderKey", "AIProviderKeyService_DeleteAIProviderKey", "AIProviderKeyService_UpdateAIProviderKey",
		"ModelService_DisableModel", "ModelService_EnableModel", "ModelService_SwapModelOnVariations",
	}
	for _, id := range shared {
		w.results[id] = result{"blocked", "Deferred by the fresh Go run for root's serialized cross-SDK account/admin/provider/model phase; no request was sent."}
	}
	specialized := []string{
		"ObjectiveService_CreateObjective", "ObjectiveService_CreateObjectiveFeedback",
		"ObjectiveService_ApproveToolCall", "ObjectiveService_DenyToolCall", "ObjectiveService_SetToolCallContent",
		"ObjectiveService_CancelObjective", "ObjectiveService_CompactObjective", "ObjectiveService_ContinueObjective",
	}
	for _, id := range specialized {
		w.results[id] = result{"blocked", "Pending the current-run Go specialized objective/MCP/SSE fixture flow; no request was sent by the owned mutation wave."}
	}
}

func fatal(err error) { fmt.Fprintln(os.Stderr, "go live-mutate runner failed:", err); os.Exit(1) }
func readResults() map[string]result {
	path := filepath.Join("..", "..", "results-go.json")
	raw, err := os.ReadFile(path)
	if err != nil {
		return map[string]result{}
	}
	var prior report
	if json.Unmarshal(raw, &prior) != nil {
		return map[string]result{}
	}
	return prior.Operations
}
func writeResults(operations map[string]result) {
	out := report{SchemaVersion: 1, SDK: "go", ExecutedAt: time.Now().UTC().Format(time.RFC3339), Operations: operations}
	raw, err := json.MarshalIndent(out, "", "  ")
	if err != nil {
		fatal(err)
	}
	raw = append(raw, '\n')
	if err := os.WriteFile(filepath.Join("..", "..", "results-go.json"), raw, 0o644); err != nil {
		fatal(err)
	}
}
func (w *wave) probe(id, evidence string, fn func(context.Context) error) bool {
	ctx, cancel := context.WithTimeout(context.Background(), 30*time.Second)
	defer cancel()
	err := fn(ctx)
	if err == nil {
		w.results[id] = result{"completed", evidence}
		return true
	}
	var apiErr *sdk.APIError
	if errors.As(err, &apiErr) {
		w.results[id] = result{"failed", fmt.Sprintf("Real API returned HTTP %d / API code %d; no body, secret, or identifier persisted.", apiErr.StatusCode, apiErr.Code)}
	} else {
		w.results[id] = result{"failed", fmt.Sprintf("Generated Go SDK returned %T; no body, secret, or identifier persisted.", err)}
	}
	return false
}
func (w *wave) block(ids []string, dependency string) {
	for _, id := range ids {
		w.results[id] = result{"blocked", "Owned " + dependency + " fixture was unavailable; no request was sent."}
	}
}
func name(run, suffix string) *sdk.CreateResourceMetadata {
	return &sdk.CreateResourceMetadata{Name: run + "-" + suffix, Labels: map[string]string{"redwood-live": "go"}}
}
func updateName(run, suffix string) *sdk.UpdateResourceMetadata {
	return &sdk.UpdateResourceMetadata{Name: run + "-" + suffix}
}

func (w *wave) runResources() {
	// API key lifecycle (the returned bearer token is deliberately never read).
	var apiKeyID string
	if w.probe("APIKeyService_CreateAPIKey", "Created uniquely named Go-owned API key; decoded metadata; token not inspected/persisted.", func(ctx context.Context) error {
		value, err := w.client.APIKeys().Create(ctx, &sdk.APIKeyCreateParams{Metadata: &sdk.CreateAccountResourceMetadata{Name: w.run + "-api-key", Labels: map[string]string{"redwood-live": "go"}}, Spec: &sdk.APIKeySpecParam{Description: sdk.String("Go live matrix"), Permissions: []string{"*"}}})
		if err != nil {
			return err
		}
		if value == nil || value.Metadata == nil || value.Metadata.ID == "" {
			return errors.New("missing API-key metadata")
		}
		apiKeyID = value.Metadata.ID
		return nil
	}) {
		w.probe("APIKeyService_UpdateAPIKey", "Updated owned API-key description and decoded response.", func(ctx context.Context) error {
			_, err := w.client.APIKeys().Update(ctx, apiKeyID, &sdk.APIKeyUpdateParams{Spec: &sdk.APIKeySpecParam{Description: sdk.String("Go live matrix updated")}, UpdateMask: sdk.String("spec.description")})
			return err
		})
		w.probe("APIKeyService_DisableAPIKey", "Disabled owned API key and decoded response.", func(ctx context.Context) error { _, err := w.client.APIKeys().Disable(ctx, apiKeyID, nil); return err })
		w.probe("APIKeyService_EnableAPIKey", "Enabled owned API key and decoded response.", func(ctx context.Context) error { _, err := w.client.APIKeys().Enable(ctx, apiKeyID, nil); return err })
		w.probe("APIKeyService_RotateAPIKey", "Rotated owned API key and decoded response; new token not inspected/persisted.", func(ctx context.Context) error { _, err := w.client.APIKeys().Rotate(ctx, apiKeyID, nil); return err })
		w.probe("APIKeyService_DeleteAPIKey", "Deleted the owned API-key fixture.", func(ctx context.Context) error { return w.client.APIKeys().Delete(ctx, apiKeyID, nil) })
	} else {
		w.block([]string{"APIKeyService_UpdateAPIKey", "APIKeyService_DisableAPIKey", "APIKeyService_EnableAPIKey", "APIKeyService_RotateAPIKey", "APIKeyService_DeleteAPIKey"}, "API key")
	}

	// Workspace secret lifecycle; values are sent but never logged or retained.
	var workspaceSecretID string
	if w.probe("WorkspaceSecretService_CreateWorkspaceSecret", "Created uniquely named Go-owned workspace secret; value not persisted.", func(ctx context.Context) error {
		value, err := w.client.WorkspaceSecrets().Create(ctx, &sdk.WorkspaceSecretCreateParams{Metadata: name(w.run, "workspace-secret"), Spec: &sdk.WorkspaceSecretSpec{Value: sdk.String("redwood-go-live-value")}})
		if err != nil {
			return err
		}
		if value == nil || value.Metadata == nil {
			return errors.New("missing secret metadata")
		}
		workspaceSecretID = value.Metadata.ID
		return nil
	}) {
		w.probe("WorkspaceSecretService_GetWorkspaceSecret", "Retrieved owned workspace-secret metadata; value/body not persisted.", func(ctx context.Context) error {
			value, err := w.client.WorkspaceSecrets().Retrieve(ctx, workspaceSecretID, nil)
			if err != nil {
				return err
			}
			if value == nil || value.Metadata == nil {
				return errors.New("secret metadata absent")
			}
			return nil
		})
		w.probe("WorkspaceSecretService_UpdateWorkspaceSecret", "Updated owned workspace secret; value not persisted.", func(ctx context.Context) error {
			_, err := w.client.WorkspaceSecrets().Update(ctx, workspaceSecretID, &sdk.WorkspaceSecretUpdateParams{Spec: &sdk.WorkspaceSecretSpec{Value: sdk.String("redwood-go-live-value-2")}, UpdateMask: sdk.String("spec.value")})
			return err
		})
		w.probe("WorkspaceSecretService_DeleteWorkspaceSecret", "Deleted owned workspace-secret fixture.", func(ctx context.Context) error {
			return w.client.WorkspaceSecrets().Delete(ctx, workspaceSecretID, nil)
		})
	} else {
		w.block([]string{"WorkspaceSecretService_UpdateWorkspaceSecret", "WorkspaceSecretService_DeleteWorkspaceSecret"}, "workspace secret")
	}

	// Tool set, nested secret, and tool lifecycle.
	bareAdapter := sdk.NewToolSetAdapterBare(sdk.ToolSetAdapter_BareVariant{Bare: &sdk.ToolSetAdapter_Bare{}})
	bareConfig := sdk.NewToolSpec_ConfigBare(sdk.ToolSpec_Config_Bare{Bare: &sdk.Config_Bare{}})
	var toolSetID, toolID, toolSecretID string
	if w.probe("ToolService_CreateToolSet", "Created uniquely named owned bare tool set and decoded metadata.", func(ctx context.Context) error {
		value, err := w.client.ToolSets().Create(ctx, &sdk.ToolSetCreateParams{Metadata: name(w.run, "tool-set"), Spec: &sdk.ToolSetSpec{Description: sdk.String("Go live matrix"), Adapter: &bareAdapter}})
		if err != nil {
			return err
		}
		if value == nil || value.Metadata == nil {
			return errors.New("missing tool-set metadata")
		}
		toolSetID = value.Metadata.ID
		return nil
	}) {
		w.probe("ToolService_UpdateToolSet", "Updated owned tool-set description and decoded response.", func(ctx context.Context) error {
			_, err := w.client.ToolSets().Update(ctx, toolSetID, &sdk.ToolSetUpdateParams{Spec: &sdk.ToolSetSpec{Description: sdk.String("Go live matrix updated")}, UpdateMask: sdk.String("spec.description")})
			return err
		})
		w.probe("ToolService_ArchiveToolSet", "Archived owned tool set and decoded response.", func(ctx context.Context) error {
			_, err := w.client.ToolSets().Archive(ctx, toolSetID, nil)
			return err
		})
		w.probe("ToolService_UnarchiveToolSet", "Unarchived owned tool set and decoded response.", func(ctx context.Context) error {
			_, err := w.client.ToolSets().Unarchive(ctx, toolSetID, nil)
			return err
		})
		if w.probe("ToolService_CreateToolSetSecret", "Created owned tool-set secret; value not persisted.", func(ctx context.Context) error {
			value, err := w.client.ToolSets().Secrets().Create(ctx, toolSetID, &sdk.ToolSetSecretCreateParams{Metadata: name(w.run, "tool-secret"), Spec: &sdk.ToolSetSecretSpec{Value: sdk.String("go-live-secret")}})
			if err != nil {
				return err
			}
			if value == nil || value.Metadata == nil {
				return errors.New("missing tool-secret metadata")
			}
			toolSecretID = value.Metadata.ID
			return nil
		}) {
			w.probe("ToolService_GetToolSetSecret", "Retrieved owned tool-set-secret metadata; value/body not persisted.", func(ctx context.Context) error {
				value, err := w.client.ToolSets().Secrets().Retrieve(ctx, toolSetID, toolSecretID, nil)
				if err != nil {
					return err
				}
				if value == nil || value.Metadata == nil {
					return errors.New("tool-secret metadata absent")
				}
				return nil
			})
			w.probe("ToolService_UpdateToolSetSecret", "Updated owned tool-set secret; value not persisted.", func(ctx context.Context) error {
				_, err := w.client.ToolSets().Secrets().Update(ctx, toolSetID, toolSecretID, &sdk.ToolSetSecretUpdateParams{Spec: &sdk.ToolSetSecretSpec{Value: sdk.String("go-live-secret-2")}, UpdateMask: sdk.String("spec.value")})
				return err
			})
			w.probe("ToolService_DeleteToolSetSecret", "Deleted owned tool-set secret.", func(ctx context.Context) error {
				return w.client.ToolSets().Secrets().Delete(ctx, toolSetID, toolSecretID, nil)
			})
		} else {
			w.block([]string{"ToolService_UpdateToolSetSecret", "ToolService_DeleteToolSetSecret"}, "tool-set secret")
		}
		if w.probe("ToolService_CreateTool", "Created uniquely named owned bare tool and decoded metadata.", func(ctx context.Context) error {
			value, err := w.client.ToolSets().Tools().Create(ctx, toolSetID, &sdk.ToolCreateParams{Metadata: name(w.run, "tool"), Spec: &sdk.ToolSpec{Description: "Go live matrix echo", RequiresApproval: false, Parameters: map[string]any{"type": "object"}, Config: &bareConfig}})
			if err != nil {
				return err
			}
			if value == nil || value.Metadata == nil {
				return errors.New("missing tool metadata")
			}
			toolID = value.Metadata.ID
			return nil
		}) {
			w.probe("ToolService_UpdateTool", "Updated owned tool description and decoded response.", func(ctx context.Context) error {
				_, err := w.client.ToolSets().Tools().Update(ctx, toolSetID, toolID, &sdk.ToolUpdateParams{Spec: &sdk.ToolSpec{Description: "Go live matrix echo updated", RequiresApproval: false, Parameters: map[string]any{"type": "object"}, Config: &bareConfig}, UpdateMask: sdk.String("spec.description")})
				return err
			})
			w.probe("ToolService_OmitTool", "Omitted owned tool and decoded response.", func(ctx context.Context) error {
				_, err := w.client.ToolSets().Tools().Omit(ctx, toolSetID, toolID, nil)
				return err
			})
			w.probe("ToolService_RestoreTool", "Restored owned tool and decoded response.", func(ctx context.Context) error {
				_, err := w.client.ToolSets().Tools().Restore(ctx, toolSetID, toolID, nil)
				return err
			})
		} else {
			w.block([]string{"ToolService_UpdateTool", "ToolService_OmitTool", "ToolService_RestoreTool"}, "tool")
		}
	} else {
		w.block([]string{"ToolService_UpdateToolSet", "ToolService_ArchiveToolSet", "ToolService_UnarchiveToolSet", "ToolService_CreateToolSetSecret", "ToolService_UpdateToolSetSecret", "ToolService_DeleteToolSetSecret", "ToolService_CreateTool", "ToolService_UpdateTool", "ToolService_OmitTool", "ToolService_RestoreTool"}, "tool set")
	}

	// Memory layer and entry lifecycle. Keep the layer alive for the agent
	// assignment probes below, then delete it after the agent is gone.
	var memoryID, entryID string
	if w.probe("MemoryService_CreateMemoryLayer", "Created uniquely named owned skills memory layer and decoded metadata.", func(ctx context.Context) error {
		value, err := w.client.MemoryLayers().Create(ctx, &sdk.MemoryLayerCreateParams{Metadata: name(w.run, "memory"), Spec: &sdk.MemoryLayerSpecParam{Type: sdk.MemoryLayerSpecTypeMemoryLayerTypeSkills, Description: sdk.String("Go live matrix")}})
		if err != nil {
			return err
		}
		if value == nil || value.Metadata == nil {
			return errors.New("missing memory metadata")
		}
		memoryID = value.Metadata.ID
		return nil
	}) {
		w.probe("MemoryService_GetMemoryLayer", "Retrieved owned memory layer and decoded metadata.", func(ctx context.Context) error {
			value, err := w.client.MemoryLayers().Retrieve(ctx, memoryID, nil)
			if err != nil {
				return err
			}
			if value == nil || value.Metadata == nil {
				return errors.New("memory metadata absent")
			}
			return nil
		})
		w.probe("MemoryService_UpdateMemoryLayer", "Updated owned memory-layer description and decoded response.", func(ctx context.Context) error {
			_, err := w.client.MemoryLayers().Update(ctx, memoryID, &sdk.MemoryLayerUpdateParams{Spec: &sdk.MemoryLayerSpecParam{Type: sdk.MemoryLayerSpecTypeMemoryLayerTypeSkills, Description: sdk.String("Go live matrix updated")}, UpdateMask: sdk.String("spec.description")})
			return err
		})
		entrySpec := sdk.NewMemoryEntryCreateSpecContent(sdk.MemoryEntryCreateSpec_Content{Content: "Go live matrix content", Key: sdk.String("go-live-entry")})
		if w.probe("MemoryService_CreateMemoryEntry", "Created owned inline memory entry and decoded metadata.", func(ctx context.Context) error {
			value, err := w.client.MemoryLayers().Entries().Create(ctx, memoryID, &sdk.MemoryEntryCreateParams{Metadata: name(w.run, "entry"), Spec: &entrySpec})
			if err != nil {
				return err
			}
			if value == nil || value.Metadata == nil {
				return errors.New("missing entry metadata")
			}
			entryID = value.Metadata.ID
			return nil
		}) {
			w.probe("MemoryService_ListMemoryEntries", "Listed entries in owned memory layer and decoded page.", func(ctx context.Context) error {
				page, err := w.client.MemoryLayers().Entries().List(ctx, memoryID, &sdk.MemoryEntryListParams{Limit: sdk.Int32(20)})
				if err != nil {
					return err
				}
				if page == nil {
					return errors.New("nil memory-entry page")
				}
				return nil
			})
			w.probe("MemoryService_GetMemoryEntry", "Retrieved owned memory entry and decoded detail metadata.", func(ctx context.Context) error {
				value, err := w.client.MemoryLayers().Entries().Retrieve(ctx, memoryID, entryID, nil)
				if err != nil {
					return err
				}
				if value == nil || value.Metadata == nil {
					return errors.New("entry metadata absent")
				}
				return nil
			})
			w.probe("MemoryService_UpdateMemoryEntry", "Updated owned memory-entry content and decoded response.", func(ctx context.Context) error {
				_, err := w.client.MemoryLayers().Entries().Update(ctx, memoryID, entryID, &sdk.MemoryEntryUpdateParams{Spec: &sdk.MemoryEntryUpdateSpec{Content: sdk.String("Go live matrix content updated")}, UpdateMask: sdk.String("spec.content")})
				return err
			})
			w.probe("MemoryService_DeleteMemoryEntry", "Deleted owned memory-entry fixture.", func(ctx context.Context) error {
				return w.client.MemoryLayers().Entries().Delete(ctx, memoryID, entryID, nil)
			})
		} else {
			w.block([]string{"MemoryService_UpdateMemoryEntry", "MemoryService_DeleteMemoryEntry"}, "memory entry")
		}
	} else {
		w.block([]string{"MemoryService_UpdateMemoryLayer", "MemoryService_CreateMemoryEntry", "MemoryService_UpdateMemoryEntry", "MemoryService_DeleteMemoryEntry"}, "memory layer")
	}

	// Discover an existing model; it is required by variation creation.
	models, modelErr := w.client.Models().List(context.Background(), &sdk.ModelListParams{Limit: sdk.Int32(50)})
	modelID := ""
	if modelErr == nil && models != nil {
		for _, m := range models.Items {
			if m.Metadata != nil && m.Metadata.ID != "" && m.State == sdk.ModelStateStateEnabled {
				modelID = m.Metadata.ID
				break
			}
		}
	}

	// Agent, variation, assignment, memory assignment, and schedule lifecycle.
	var agentID, defaultVariationID, extraVariationID, assignmentID, memoryAssignmentID, scheduleID string
	if modelID == "" {
		w.block([]string{"AgentService_CreateAgent", "AgentService_UpdateAgent", "AgentService_PublishAgent", "AgentService_UnpublishAgent", "AgentService_ArchiveAgent", "AgentService_UnarchiveAgent", "AgentService_DeleteAgent", "AgentVariationService_CreateAgentVariation", "AgentVariationService_UpdateAgentVariation", "AgentVariationService_DeleteAgentVariation", "AgentVariationService_AddAgentVariationAssignment", "AgentVariationService_RemoveAgentVariationAssignment", "AgentVariationService_AddAgentVariationMemoryLayer", "AgentVariationService_UpdateAgentVariationMemoryLayer", "AgentVariationService_RemoveAgentVariationMemoryLayer", "AgentScheduleService_CreateAgentSchedule", "AgentScheduleService_UpdateAgentSchedule", "AgentScheduleService_PauseAgentSchedule", "AgentScheduleService_ResumeAgentSchedule", "AgentScheduleService_ArchiveAgentSchedule", "AgentScheduleService_DeleteAgentSchedule"}, "enabled model")
	} else {
		variationSpec := &sdk.AgentVariationSpec{SystemPromptTemplate: sdk.String("You are a Go live matrix fixture."), FirstUserMessageTemplate: sdk.String("Reply OK."), ModelConfig: &sdk.AgentVariationSpec_ModelConfig{ModelID: sdk.String(modelID)}}
		if w.probe("AgentService_CreateAgent", "Created uniquely named owned agent with default variation and decoded metadata.", func(ctx context.Context) error {
			value, err := w.client.Agents().Create(ctx, &sdk.AgentCreateParams{Metadata: name(w.run, "agent"), Spec: &sdk.AgentSpec{VariationSelectionMode: sdk.AgentSpecVariationSelectionModeVariationSelectionModeRandom}, DefaultVariation: &sdk.CreateAgentVariationRequestParam{Metadata: name(w.run, "variation-default"), Spec: variationSpec}})
			if err != nil {
				return err
			}
			if value == nil || value.Metadata == nil {
				return errors.New("missing agent metadata")
			}
			agentID = value.Metadata.ID
			page, e := w.client.Agents().Variations().List(ctx, agentID, &sdk.AgentVariationListParams{Limit: sdk.Int32(20)})
			if e != nil {
				return e
			}
			if page == nil || len(page.Items) == 0 || page.Items[0].Metadata == nil {
				return errors.New("default variation unavailable")
			}
			defaultVariationID = page.Items[0].Metadata.ID
			return nil
		}) {
			w.probe("AgentService_UpdateAgent", "Updated owned agent description and decoded response.", func(ctx context.Context) error {
				_, err := w.client.Agents().Update(ctx, agentID, &sdk.AgentUpdateParams{Spec: &sdk.AgentSpec{Description: sdk.String("Go live matrix updated"), VariationSelectionMode: sdk.AgentSpecVariationSelectionModeVariationSelectionModeRandom}, UpdateMask: sdk.String("spec.description")})
				return err
			})
			if w.probe("AgentVariationService_CreateAgentVariation", "Created second owned agent variation and decoded metadata.", func(ctx context.Context) error {
				value, err := w.client.Agents().Variations().Create(ctx, agentID, &sdk.AgentVariationCreateParams{Metadata: name(w.run, "variation-extra"), Spec: variationSpec})
				if err != nil {
					return err
				}
				if value == nil || value.Metadata == nil {
					return errors.New("missing variation metadata")
				}
				extraVariationID = value.Metadata.ID
				return nil
			}) {
				w.probe("AgentVariationService_UpdateAgentVariation", "Updated owned variation description and decoded response.", func(ctx context.Context) error {
					_, err := w.client.Agents().Variations().Update(ctx, agentID, extraVariationID, &sdk.AgentVariationUpdateParams{Spec: &sdk.AgentVariationSpec{Description: sdk.String("Go live variation updated")}, UpdateMask: sdk.String("spec.description")})
					return err
				})
				w.probe("AgentVariationService_DeleteAgentVariation", "Deleted owned extra variation.", func(ctx context.Context) error {
					return w.client.Agents().Variations().Delete(ctx, agentID, extraVariationID, nil)
				})
			} else {
				w.block([]string{"AgentVariationService_UpdateAgentVariation", "AgentVariationService_DeleteAgentVariation"}, "extra variation")
			}
			if toolID != "" {
				body := sdk.NewAddAgentVariationAssignmentRequestParamToolID(sdk.AddAgentVariationAssignmentRequest_ToolIDParam{Type: "toolId", ToolID: toolID})
				if w.probe("AgentVariationService_AddAgentVariationAssignment", "Assigned owned tool to owned variation through whole-body union and decoded assignment.", func(ctx context.Context) error {
					value, err := w.client.Agents().Variations().AddAssignment(ctx, agentID, defaultVariationID, &sdk.AgentVariationAddAssignmentParams{Body: &body})
					if err != nil {
						return err
					}
					if value.Tool != nil && value.Tool.ID != nil {
						assignmentID = *value.Tool.ID
					}
					if assignmentID == "" {
						return errors.New("assignment id absent")
					}
					return nil
				}) {
					w.probe("AgentVariationService_RemoveAgentVariationAssignment", "Removed owned tool assignment.", func(ctx context.Context) error {
						return w.client.Agents().Variations().RemoveAssignment(ctx, agentID, defaultVariationID, assignmentID, nil)
					})
				} else {
					w.block([]string{"AgentVariationService_RemoveAgentVariationAssignment"}, "assignment")
				}
			} else {
				w.block([]string{"AgentVariationService_AddAgentVariationAssignment", "AgentVariationService_RemoveAgentVariationAssignment"}, "tool")
			}
			if memoryID != "" {
				if w.probe("AgentVariationService_AddAgentVariationMemoryLayer", "Attached owned memory layer to owned variation and decoded assignment.", func(ctx context.Context) error {
					value, err := w.client.Agents().Variations().AddMemoryLayer(ctx, agentID, defaultVariationID, &sdk.AgentVariationAddMemoryLayerParams{MemoryLayerID: memoryID, Position: sdk.Int32(7)})
					if err != nil {
						return err
					}
					if value == nil || value.ID == nil {
						return errors.New("memory assignment id absent")
					}
					memoryAssignmentID = *value.ID
					return nil
				}) {
					w.probe("AgentVariationService_UpdateAgentVariationMemoryLayer", "Updated owned memory assignment position and decoded response.", func(ctx context.Context) error {
						_, err := w.client.Agents().Variations().UpdateMemoryLayer(ctx, agentID, defaultVariationID, memoryAssignmentID, &sdk.AgentVariationUpdateMemoryLayerParams{Position: sdk.Int32(8)})
						return err
					})
					w.probe("AgentVariationService_RemoveAgentVariationMemoryLayer", "Removed owned memory-layer assignment.", func(ctx context.Context) error {
						return w.client.Agents().Variations().RemoveMemoryLayer(ctx, agentID, defaultVariationID, memoryAssignmentID, nil)
					})
				} else {
					w.block([]string{"AgentVariationService_UpdateAgentVariationMemoryLayer", "AgentVariationService_RemoveAgentVariationMemoryLayer"}, "memory assignment")
				}
			} else {
				w.block([]string{"AgentVariationService_AddAgentVariationMemoryLayer", "AgentVariationService_UpdateAgentVariationMemoryLayer", "AgentVariationService_RemoveAgentVariationMemoryLayer"}, "memory layer")
			}
			w.probe("AgentService_PublishAgent", "Published owned agent and decoded response.", func(ctx context.Context) error { _, err := w.client.Agents().Publish(ctx, agentID, nil); return err })
			scheduleSpec := &sdk.AgentScheduleSpec{Schedule: &sdk.AgentScheduleSpec_Schedule{Intervals: []sdk.Schedule_Interval{{Every: sdk.String("86400s")}}, Timezone: sdk.String("UTC")}, VariationID: sdk.String(defaultVariationID), FirstUserMessage: sdk.String("Reply OK.")}
			if w.probe("AgentScheduleService_CreateAgentSchedule", "Created owned long-interval schedule and decoded metadata.", func(ctx context.Context) error {
				value, err := w.client.Agents().Schedules().Create(ctx, agentID, &sdk.AgentScheduleCreateParams{Metadata: name(w.run, "schedule"), Spec: scheduleSpec})
				if err != nil {
					return err
				}
				if value == nil || value.Metadata == nil {
					return errors.New("schedule metadata absent")
				}
				scheduleID = value.Metadata.ID
				return nil
			}) {
				w.probe("AgentScheduleService_GetAgentSchedule", "Retrieved owned schedule and decoded metadata.", func(ctx context.Context) error {
					value, err := w.client.Agents().Schedules().Retrieve(ctx, agentID, scheduleID, nil)
					if err != nil {
						return err
					}
					if value == nil || value.Metadata == nil {
						return errors.New("schedule metadata absent")
					}
					return nil
				})
				w.probe("AgentScheduleService_UpdateAgentSchedule", "Updated owned schedule name and decoded response.", func(ctx context.Context) error {
					_, err := w.client.Agents().Schedules().Update(ctx, agentID, scheduleID, &sdk.AgentScheduleUpdateParams{Metadata: updateName(w.run, "schedule-updated"), UpdateMask: sdk.String("metadata.name")})
					return err
				})
				w.probe("AgentScheduleService_PauseAgentSchedule", "Paused owned schedule and decoded response.", func(ctx context.Context) error {
					_, err := w.client.Agents().Schedules().Pause(ctx, agentID, scheduleID, nil)
					return err
				})
				w.probe("AgentScheduleService_ResumeAgentSchedule", "Resumed owned schedule and decoded response.", func(ctx context.Context) error {
					_, err := w.client.Agents().Schedules().Resume(ctx, agentID, scheduleID, nil)
					return err
				})
				w.probe("AgentScheduleService_ArchiveAgentSchedule", "Archived owned schedule and decoded response.", func(ctx context.Context) error {
					_, err := w.client.Agents().Schedules().Archive(ctx, agentID, scheduleID, nil)
					return err
				})
				w.probe("AgentScheduleService_DeleteAgentSchedule", "Deleted owned schedule.", func(ctx context.Context) error {
					return w.client.Agents().Schedules().Delete(ctx, agentID, scheduleID, nil)
				})
			} else {
				w.block([]string{"AgentScheduleService_UpdateAgentSchedule", "AgentScheduleService_PauseAgentSchedule", "AgentScheduleService_ResumeAgentSchedule", "AgentScheduleService_ArchiveAgentSchedule", "AgentScheduleService_DeleteAgentSchedule"}, "schedule")
			}
			w.runWidgets(agentID, defaultVariationID)
			w.probe("AgentService_UnpublishAgent", "Unpublished owned agent and decoded response.", func(ctx context.Context) error { _, err := w.client.Agents().Unpublish(ctx, agentID, nil); return err })
			w.probe("AgentService_ArchiveAgent", "Archived owned agent and decoded response.", func(ctx context.Context) error { _, err := w.client.Agents().Archive(ctx, agentID, nil); return err })
			w.probe("AgentService_UnarchiveAgent", "Unarchived owned agent and decoded response.", func(ctx context.Context) error { _, err := w.client.Agents().Unarchive(ctx, agentID, nil); return err })
			w.probe("AgentService_DeleteAgent", "Deleted owned agent fixture.", func(ctx context.Context) error { return w.client.Agents().Delete(ctx, agentID, nil) })
		} else {
			w.block([]string{"AgentService_UpdateAgent", "AgentService_PublishAgent", "AgentService_UnpublishAgent", "AgentService_ArchiveAgent", "AgentService_UnarchiveAgent", "AgentService_DeleteAgent"}, "agent")
		}
	}

	if memoryID != "" {
		w.probe("MemoryService_DeleteMemoryLayer", "Deleted owned memory-layer fixture.", func(ctx context.Context) error { return w.client.MemoryLayers().Delete(ctx, memoryID, nil) })
	} else {
		w.block([]string{"MemoryService_DeleteMemoryLayer"}, "memory layer")
	}
	if toolID != "" {
		w.probe("ToolService_DeleteTool", "Deleted owned tool fixture.", func(ctx context.Context) error {
			return w.client.ToolSets().Tools().Delete(ctx, toolSetID, toolID, nil)
		})
	} else {
		w.block([]string{"ToolService_DeleteTool"}, "tool")
	}
	if toolSetID != "" {
		w.probe("ToolService_DeleteToolSet", "Deleted owned tool-set fixture.", func(ctx context.Context) error { return w.client.ToolSets().Delete(ctx, toolSetID, nil) })
	} else {
		w.block([]string{"ToolService_DeleteToolSet"}, "tool set")
	}

	// Upload creation has no delete endpoint. The unique run label makes its
	// eventual expiration auditable. Retrieve immediately to cover both calls.
	var uploadID string
	if w.probe("UploadService_CreateUpload", "Created uniquely named one-byte upload and decoded presigned-upload metadata; URL/body not persisted.", func(ctx context.Context) error {
		value, err := w.client.Uploads().Create(ctx, &sdk.UploadCreateParams{Metadata: name(w.run, "upload"), Spec: &sdk.UploadSpec{Filename: "one-byte.txt", ContentType: "text/plain", SizeBytes: "1"}})
		if err != nil {
			return err
		}
		if value == nil || value.Metadata == nil {
			return errors.New("upload metadata absent")
		}
		uploadID = value.Metadata.ID
		return nil
	}) {
		w.probe("UploadService_GetUpload", "Real API 200; retrieved the owned upload and decoded metadata; URL/body not persisted.", func(ctx context.Context) error {
			value, err := w.client.Uploads().Retrieve(ctx, uploadID, nil)
			if err != nil {
				return err
			}
			if value == nil || value.Metadata == nil {
				return errors.New("upload metadata absent")
			}
			return nil
		})
	}
}

func (w *wave) runWidgets(agentID, variationID string) {
	var widgetID, session1ID, session2ID, tenantID string
	if !w.probe("WidgetService_CreateWidget", "Created uniquely named owned widget and decoded metadata.", func(ctx context.Context) error {
		value, err := w.client.Widgets().Create(ctx, &sdk.WidgetCreateParams{Metadata: name(w.run, "widget"), Spec: &sdk.WidgetSpec{AgentID: agentID, VariationID: sdk.String(variationID), OriginAllowlist: []string{"https://example.test"}}})
		if err != nil {
			return err
		}
		if value == nil || value.Metadata == nil {
			return errors.New("widget metadata absent")
		}
		widgetID = value.Metadata.ID
		return nil
	}) {
		w.block([]string{"WidgetService_GetWidget", "WidgetService_UpdateWidget", "WidgetService_ArchiveWidget", "WidgetService_UnarchiveWidget", "WidgetService_DeleteWidget", "WidgetSessionService_CreateWidgetSession", "WidgetSessionService_RevokeWidgetSession", "WidgetSessionService_DeleteWidgetSession", "WidgetSessionService_DeleteTenantWidgetSessions", "TenantService_DeleteTenant"}, "widget")
		return
	}
	w.probe("WidgetService_GetWidget", "Retrieved owned widget and decoded metadata.", func(ctx context.Context) error {
		value, err := w.client.Widgets().Retrieve(ctx, widgetID, nil)
		if err != nil {
			return err
		}
		if value == nil || value.Metadata == nil {
			return errors.New("widget metadata absent")
		}
		return nil
	})
	w.probe("WidgetService_UpdateWidget", "Updated owned widget origins and decoded response.", func(ctx context.Context) error {
		_, err := w.client.Widgets().Update(ctx, widgetID, &sdk.WidgetUpdateParams{Spec: &sdk.WidgetSpec{AgentID: agentID, VariationID: sdk.String(variationID), OriginAllowlist: []string{"https://example.test", "https://example.org"}}, UpdateMask: sdk.String("spec.origin_allowlist")})
		return err
	})
	w.probe("WidgetService_ArchiveWidget", "Archived owned widget and decoded response.", func(ctx context.Context) error { _, err := w.client.Widgets().Archive(ctx, widgetID, nil); return err })
	w.probe("WidgetService_UnarchiveWidget", "Unarchived owned widget and decoded response.", func(ctx context.Context) error {
		_, err := w.client.Widgets().Unarchive(ctx, widgetID, nil)
		return err
	})
	tenantExternal := w.run + "-tenant"
	createSession := func(ctx context.Context) (*sdk.WidgetSession, error) {
		return w.client.WidgetSessions().Create(ctx, &sdk.WidgetSessionCreateParams{Spec: &sdk.WidgetSessionSpecParam{WidgetID: widgetID, Tenant: &sdk.TenantAssertion{ID: tenantExternal, Name: sdk.String("Go live tenant")}, Subject: &sdk.SubjectAssertion{ID: w.run + "-subject", Name: sdk.String("Go live subject")}}})
	}
	if w.probe("WidgetSessionService_CreateWidgetSession", "Created owned widget session with unique tenant/subject and decoded metadata; token/body not persisted.", func(ctx context.Context) error {
		value, err := createSession(ctx)
		if err != nil {
			return err
		}
		if value == nil || value.Metadata == nil {
			return errors.New("session metadata absent")
		}
		session1ID = value.Metadata.ID
		if value.Info != nil && value.Info.Tenant != nil {
			tenantID = value.Info.Tenant.ID
		}
		return nil
	}) {
		w.probe("WidgetSessionService_GetWidgetSession", "Retrieved owned widget session and decoded metadata; token/body not persisted.", func(ctx context.Context) error {
			value, err := w.client.WidgetSessions().Retrieve(ctx, session1ID, nil)
			if err != nil {
				return err
			}
			if value == nil || value.Metadata == nil {
				return errors.New("session metadata absent")
			}
			return nil
		})
		w.probe("WidgetSessionService_RevokeWidgetSession", "Revoked owned widget session and decoded response.", func(ctx context.Context) error {
			_, err := w.client.WidgetSessions().Revoke(ctx, session1ID, nil)
			return err
		})
		w.probe("WidgetSessionService_DeleteWidgetSession", "Deleted owned widget session.", func(ctx context.Context) error { return w.client.WidgetSessions().Delete(ctx, session1ID, nil) })
	} else {
		w.block([]string{"WidgetSessionService_RevokeWidgetSession", "WidgetSessionService_DeleteWidgetSession"}, "widget session")
	}
	// A second owned session makes tenant-wide deletion observable.
	ctx, cancel := context.WithTimeout(context.Background(), 30*time.Second)
	second, err := createSession(ctx)
	cancel()
	if err == nil && second != nil && second.Metadata != nil {
		session2ID = second.Metadata.ID
		if tenantID == "" && second.Info != nil && second.Info.Tenant != nil {
			tenantID = second.Info.Tenant.ID
		}
	}
	if session2ID != "" && tenantID != "" {
		w.probe("WidgetSessionService_DeleteTenantWidgetSessions", "Deleted all sessions for the unique owned tenant and decoded response.", func(ctx context.Context) error {
			_, err := w.client.WidgetSessions().DeleteTenant(ctx, &sdk.WidgetSessionDeleteTenantParams{TenantID: sdk.String(tenantID)})
			return err
		})
	} else {
		w.block([]string{"WidgetSessionService_DeleteTenantWidgetSessions"}, "second tenant widget session")
	}
	if tenantID != "" {
		w.probe("TenantService_GetTenant", "Retrieved unique run-owned tenant and decoded metadata.", func(ctx context.Context) error {
			value, err := w.client.Tenants().Retrieve(ctx, tenantID, nil)
			if err != nil {
				return err
			}
			if value == nil || value.Metadata == nil {
				return errors.New("tenant metadata absent")
			}
			return nil
		})
		w.probe("TenantService_ListTenantSubjects", "Listed subjects for unique run-owned tenant and decoded page.", func(ctx context.Context) error {
			page, err := w.client.Tenants().ListSubjects(ctx, tenantID, &sdk.TenantListSubjectsParams{Limit: sdk.Int32(20)})
			if err != nil {
				return err
			}
			if page == nil {
				return errors.New("nil subject page")
			}
			return nil
		})
		w.probe("TenantService_DeleteTenant", "Deleted the unique tenant asserted only by this Go run and decoded response.", func(ctx context.Context) error { _, err := w.client.Tenants().Delete(ctx, tenantID, nil); return err })
	} else {
		w.block([]string{"TenantService_DeleteTenant"}, "tenant")
	}
	w.probe("WidgetService_DeleteWidget", "Deleted owned widget fixture.", func(ctx context.Context) error { return w.client.Widgets().Delete(ctx, widgetID, nil) })
}

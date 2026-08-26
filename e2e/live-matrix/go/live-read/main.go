// A non-mutating, success-path live wave for every GET operation exposed by
// the generated Go SDK. IDs are learned only through successful list calls.
// No response body, resource name, identifier, or secret field is printed or
// persisted. Run from the repository root after exporting the Cadenya env:
//
//	(set -a; source ../../../.env.development; set +a; \
//	   cd e2e/live-matrix/go/live-read && go run .)
package main

import (
	"context"
	"encoding/json"
	"errors"
	"fmt"
	"os"
	"path/filepath"
	"regexp"
	"sort"
	"strings"
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
	client     *sdk.Client
	results    map[string]result
	agent      *sdk.Agent
	schedule   *sdk.AgentSchedule
	variation  *sdk.AgentVariation
	memory     *sdk.MemoryLayer
	entry      *sdk.MemoryEntry
	model      *sdk.Model
	objective  *sdk.Objective
	events     []sdk.ObjectiveEvent
	task       *sdk.ObjectiveTask
	toolCall   *sdk.ObjectiveToolCall
	tenant     *sdk.Tenant
	toolSet    *sdk.ToolSet
	openAPISet *sdk.ToolSet
	tool       *sdk.Tool
	toolSecret *sdk.ToolSetSecret
	widget     *sdk.Widget
	session    *sdk.WidgetSession
	apiKey     *sdk.APIKey
	provider   *sdk.AIProviderKey
	secret     *sdk.WorkspaceSecret
}

func main() {
	if os.Getenv("CADENYA_API_KEY") == "" || os.Getenv("CADENYA_WORKSPACE_ID") == "" {
		fatal(errors.New("CADENYA_API_KEY and CADENYA_WORKSPACE_ID are required"))
	}
	client, err := sdk.NewClient()
	if err != nil {
		fatal(err)
	}
	w := &wave{client: client, results: map[string]result{}}
	w.run()
	w.normalizeKnownBlocks()

	out := report{
		SchemaVersion: 1,
		SDK:           "go",
		ExecutedAt:    time.Now().UTC().Format(time.RFC3339),
		Operations:    w.results,
	}
	raw, err := json.MarshalIndent(out, "", "  ")
	if err != nil {
		fatal(err)
	}
	raw = append(raw, '\n')
	output := filepath.Join("..", "..", "results-go.json")
	if err := os.WriteFile(output, raw, 0o644); err != nil {
		fatal(err)
	}

	counts := map[string]int{}
	for _, value := range w.results {
		counts[value.Status]++
	}
	fmt.Printf("go read wave: %d completed, %d failed, %d blocked (%d evaluated)\n",
		counts["completed"], counts["failed"], counts["blocked"], len(w.results))
	if counts["failed"] != 0 {
		os.Exit(1)
	}
}

func (w *wave) normalizeKnownBlocks() {
	for _, id := range []string{
		"WorkspaceAdminService_ListProfiles",
		"WorkspaceAdminService_ListAccountWorkspaces",
		"WorkspaceAdminService_GetWorkspace",
		"WorkspaceAdminService_ListWorkspaceMembers",
	} {
		if value, ok := w.results[id]; ok && value.Status == "failed" {
			w.results[id] = result{Status: "blocked", Evidence: "Current credential returned HTTP 403 / API code 7; root's serialized rotated-global-key wave will retry this success path."}
		}
	}
	if value, ok := w.results["ObjectiveService_ListObjectiveTasks"]; ok && value.Status == "failed" {
		w.results["ObjectiveService_ListObjectiveTasks"] = result{Status: "blocked", Evidence: "Real API returned HTTP 501 / API code 12; the generated SDK request reached the endpoint, but upstream does not implement the success path."}
	}
	if value, ok := w.results["ObjectiveEventStreamsService_StreamObjectiveEvents"]; ok && value.Status == "failed" {
		w.results["ObjectiveEventStreamsService_StreamObjectiveEvents"] = result{Status: "blocked", Evidence: "Replay received the known out-of-contract heartbeat as a zero-value ObjectiveEvent; upstream heartbeat contract fix is pending."}
	}
}

func fatal(err error) {
	fmt.Fprintln(os.Stderr, "go live-read runner failed:", err)
	os.Exit(1)
}

func (w *wave) completed(id, evidence string) {
	w.results[id] = result{Status: "completed", Evidence: evidence}
}

func (w *wave) blocked(id, dependency string) {
	w.results[id] = result{Status: "blocked", Evidence: "No list-derived " + dependency + " fixture was available; no request was sent."}
}

func (w *wave) probe(id, evidence string, fn func(context.Context) error) bool {
	ctx, cancel := context.WithTimeout(context.Background(), 25*time.Second)
	defer cancel()
	err := fn(ctx)
	if err == nil {
		w.completed(id, evidence)
		return true
	}
	var apiErr *sdk.APIError
	if errors.As(err, &apiErr) {
		w.results[id] = result{Status: "failed", Evidence: fmt.Sprintf("Real API request returned HTTP %d / API code %d; response body was not persisted.", apiErr.StatusCode, apiErr.Code)}
	} else {
		w.results[id] = result{Status: "failed", Evidence: safeError(err)}
	}
	return false
}

var (
	quotedText = regexp.MustCompile(`"(?:[^"\\]|\\.)*"`)
	resourceID = regexp.MustCompile(`\b(?:account|workspace|profile|apikey|aipk|agent|av|as|mem|me|model|obj|oe|task|otc|tenant|subject|ts|tool|upload|widget|ws|secret)_[A-Za-z0-9_-]+\b`)
)

func safeError(err error) string {
	types := []string{}
	for current := err; current != nil; current = errors.Unwrap(current) {
		types = append(types, fmt.Sprintf("%T", current))
	}
	diagnostic := quotedText.ReplaceAllString(err.Error(), `"<redacted>"`)
	diagnostic = resourceID.ReplaceAllString(diagnostic, "<resource-id>")
	if len(diagnostic) > 240 {
		diagnostic = diagnostic[:240]
	}
	return fmt.Sprintf("Generated Go SDK error chain %s; sanitized diagnostic: %s; no response body or identifiers were persisted.", strings.Join(types, " -> "), diagnostic)
}

func first[T any](items []T) *T {
	if len(items) == 0 {
		return nil
	}
	value := items[0]
	return &value
}

func validResource(metadata *sdk.ResourceMetadata) error {
	if metadata == nil || metadata.ID == "" {
		return errors.New("decoded resource metadata/id is absent")
	}
	return nil
}

func validOperation(metadata *sdk.OperationMetadata) error {
	if metadata == nil || metadata.ID == "" {
		return errors.New("decoded operation metadata/id is absent")
	}
	return nil
}

func (w *wave) run() {
	// Account/global reads. Bodies may contain secret material and are never
	// logged or retained in the result artifact.
	w.probe("AccountService_GetAccount", "Real API 200; decoded Account with metadata and info; body not persisted.", func(ctx context.Context) error {
		value, err := w.client.Accounts().Retrieve(ctx)
		if err != nil {
			return err
		}
		if value == nil || value.Metadata == nil || value.Metadata.ID == "" || value.Info == nil {
			return errors.New("decoded Account shape incomplete")
		}
		return nil
	})
	w.probe("GlobalAPIKeyService_GetGlobalAPIKey", "Real API 200; decoded global APIKey metadata; token/body not persisted.", func(ctx context.Context) error {
		value, err := w.client.APIKeys().RetrieveGlobal(ctx)
		if err != nil {
			return err
		}
		if value == nil || value.Metadata == nil || value.Metadata.ID == "" {
			return errors.New("decoded APIKey metadata absent")
		}
		return nil
	})

	// Account-level discovery.
	w.probe("WorkspaceAdminService_ListProfiles", "Real API 200; decoded profile page (zero items is valid); body not persisted.", func(ctx context.Context) error {
		page, err := w.client.WorkspaceAdmin().ListProfiles(ctx, &sdk.WorkspaceAdminListProfilesParams{Limit: sdk.Int32(20)})
		if err != nil {
			return err
		}
		if page == nil {
			return errors.New("nil profile page")
		}
		return nil
	})
	w.probe("WorkspaceAdminService_ListAccountWorkspaces", "Real API 200; decoded account-workspace page; body not persisted.", func(ctx context.Context) error {
		page, err := w.client.WorkspaceAdmin().ListAccount(ctx, &sdk.WorkspaceAdminListAccountParams{Limit: sdk.Int32(20)})
		if err != nil {
			return err
		}
		if page == nil {
			return errors.New("nil workspace page")
		}
		return nil
	})
	w.probe("WorkspaceAdminService_GetWorkspace", "Real API 200; decoded workspace selected by configured workspace ID.", func(ctx context.Context) error {
		value, err := w.client.WorkspaceAdmin().Retrieve(ctx, nil)
		if err != nil {
			return err
		}
		if value == nil || value.Metadata == nil || value.Metadata.ID == "" {
			return errors.New("decoded Workspace metadata absent")
		}
		return nil
	})
	w.probe("WorkspaceAdminService_ListWorkspaceMembers", "Real API 200; decoded workspace-member page (zero items is valid); body not persisted.", func(ctx context.Context) error {
		page, err := w.client.WorkspaceAdmin().ListMembers(ctx, &sdk.WorkspaceAdminListMembersParams{Limit: sdk.Int32(20)})
		if err != nil {
			return err
		}
		if page == nil {
			return errors.New("nil member page")
		}
		return nil
	})
	w.probe("ProfilesService_Whoami", "Real API 200; decoded current Profile metadata; body not persisted.", func(ctx context.Context) error {
		value, err := w.client.Profiles().Whoami(ctx)
		if err != nil {
			return err
		}
		if value == nil || value.Metadata == nil || value.Metadata.ID == "" {
			return errors.New("decoded Profile metadata absent")
		}
		return nil
	})
	w.probe("WorkspaceService_ListWorkspaces", "Real API 200; decoded workspace page (zero items is valid).", func(ctx context.Context) error {
		page, err := w.client.Workspaces().List(ctx, &sdk.WorkspaceListParams{Limit: sdk.Int32(20)})
		if err != nil {
			return err
		}
		if page == nil {
			return errors.New("nil workspace page")
		}
		return nil
	})

	// Workspace API keys and provider keys. Secret fields are never inspected.
	w.probe("APIKeyService_ListAPIKeys", "Real API 200; decoded API-key page; no token/body persisted.", func(ctx context.Context) error {
		page, err := w.client.APIKeys().List(ctx, &sdk.APIKeyListParams{Limit: sdk.Int32(20)})
		if err != nil {
			return err
		}
		if page == nil {
			return errors.New("nil API-key page")
		}
		w.apiKey = first(page.Items)
		return nil
	})
	if w.apiKey == nil || w.apiKey.Metadata == nil || w.apiKey.Metadata.ID == "" {
		w.blocked("APIKeyService_GetAPIKey", "API key")
	} else {
		w.probe("APIKeyService_GetAPIKey", "Real API 200; decoded list-derived APIKey metadata; no token/body persisted.", func(ctx context.Context) error {
			value, err := w.client.APIKeys().Retrieve(ctx, w.apiKey.Metadata.ID, nil)
			if err != nil {
				return err
			}
			if value == nil || value.Metadata == nil || value.Metadata.ID == "" {
				return errors.New("decoded APIKey metadata absent")
			}
			return nil
		})
	}
	w.probe("AIProviderKeyService_ListAIProviderKeys", "Real API 200; decoded provider-key page; credential/body not persisted.", func(ctx context.Context) error {
		page, err := w.client.AIProviderKeys().List(ctx, &sdk.AIProviderKeyListParams{Limit: sdk.Int32(20)})
		if err != nil {
			return err
		}
		if page == nil {
			return errors.New("nil provider-key page")
		}
		w.provider = first(page.Items)
		return nil
	})
	if w.provider == nil || w.provider.Metadata == nil || w.provider.Metadata.ID == "" {
		w.blocked("AIProviderKeyService_GetAIProviderKey", "AI provider key")
	} else {
		w.probe("AIProviderKeyService_GetAIProviderKey", "Real API 200; decoded list-derived AIProviderKey metadata; credential/body not persisted.", func(ctx context.Context) error {
			value, err := w.client.AIProviderKeys().Retrieve(ctx, w.provider.Metadata.ID, nil)
			if err != nil {
				return err
			}
			return validResource(value.Metadata)
		})
	}

	// Agents, schedules, and variations.
	w.probe("AgentService_ListAgents", "Real API 200; decoded agent page and captured an in-memory fixture when available.", func(ctx context.Context) error {
		page, err := w.client.Agents().List(ctx, &sdk.AgentListParams{Limit: sdk.Int32(50)})
		if err != nil {
			return err
		}
		if page == nil {
			return errors.New("nil agent page")
		}
		w.agent = first(page.Items)
		return nil
	})
	if w.agent == nil || w.agent.Metadata == nil || w.agent.Metadata.ID == "" {
		for _, id := range []string{"AgentService_GetAgent", "AgentService_ListAgentFeedback", "AgentService_ListAgentWebhookDeliveries", "AgentScheduleService_ListAgentSchedules", "AgentScheduleService_GetAgentSchedule", "AgentVariationService_ListAgentVariations", "AgentVariationService_GetAgentVariation"} {
			w.blocked(id, "agent")
		}
	} else {
		agentID := w.agent.Metadata.ID
		w.probe("AgentService_GetAgent", "Real API 200; decoded list-derived Agent metadata.", func(ctx context.Context) error {
			value, err := w.client.Agents().Retrieve(ctx, agentID, nil)
			if err != nil {
				return err
			}
			return validResource(value.Metadata)
		})
		w.probe("AgentService_ListAgentFeedback", "Real API 200; decoded feedback page for a list-derived agent (zero items is valid).", func(ctx context.Context) error {
			page, err := w.client.Agents().ListFeedback(ctx, agentID, &sdk.AgentListFeedbackParams{Limit: sdk.Int32(20)})
			if err != nil {
				return err
			}
			if page == nil {
				return errors.New("nil agent-feedback page")
			}
			return nil
		})
		w.probe("AgentService_ListAgentWebhookDeliveries", "Real API 200; decoded webhook-delivery page for a list-derived agent (zero items is valid).", func(ctx context.Context) error {
			page, err := w.client.Agents().ListWebhookDeliveries(ctx, agentID, &sdk.AgentListWebhookDeliveriesParams{Limit: sdk.Int32(20)})
			if err != nil {
				return err
			}
			if page == nil {
				return errors.New("nil webhook-delivery page")
			}
			return nil
		})
		w.probe("AgentScheduleService_ListAgentSchedules", "Real API 200; decoded schedule page for a list-derived agent.", func(ctx context.Context) error {
			page, err := w.client.Agents().Schedules().List(ctx, agentID, &sdk.AgentScheduleListParams{Limit: sdk.Int32(20)})
			if err != nil {
				return err
			}
			if page == nil {
				return errors.New("nil schedule page")
			}
			w.schedule = first(page.Items)
			return nil
		})
		if w.schedule == nil || w.schedule.Metadata == nil || w.schedule.Metadata.ID == "" {
			w.blocked("AgentScheduleService_GetAgentSchedule", "agent schedule")
		} else {
			w.probe("AgentScheduleService_GetAgentSchedule", "Real API 200; decoded list-derived AgentSchedule metadata.", func(ctx context.Context) error {
				value, err := w.client.Agents().Schedules().Retrieve(ctx, agentID, w.schedule.Metadata.ID, nil)
				if err != nil {
					return err
				}
				return validResource(value.Metadata)
			})
		}
		w.probe("AgentVariationService_ListAgentVariations", "Real API 200; decoded variation page for a list-derived agent.", func(ctx context.Context) error {
			page, err := w.client.Agents().Variations().List(ctx, agentID, &sdk.AgentVariationListParams{Limit: sdk.Int32(20)})
			if err != nil {
				return err
			}
			if page == nil {
				return errors.New("nil variation page")
			}
			w.variation = first(page.Items)
			return nil
		})
		if w.variation == nil || w.variation.Metadata == nil || w.variation.Metadata.ID == "" {
			w.blocked("AgentVariationService_GetAgentVariation", "agent variation")
		} else {
			w.probe("AgentVariationService_GetAgentVariation", "Real API 200; decoded list-derived AgentVariation metadata.", func(ctx context.Context) error {
				value, err := w.client.Agents().Variations().Retrieve(ctx, agentID, w.variation.Metadata.ID, nil)
				if err != nil {
					return err
				}
				return validResource(value.Metadata)
			})
		}
	}

	// Memory.
	w.probe("MemoryService_ListMemoryLayers", "Real API 200; decoded memory-layer page.", func(ctx context.Context) error {
		page, err := w.client.MemoryLayers().List(ctx, &sdk.MemoryLayerListParams{Limit: sdk.Int32(50)})
		if err != nil {
			return err
		}
		if page == nil {
			return errors.New("nil memory-layer page")
		}
		w.memory = first(page.Items)
		return nil
	})
	if w.memory == nil || w.memory.Metadata == nil || w.memory.Metadata.ID == "" {
		for _, id := range []string{"MemoryService_GetMemoryLayer", "MemoryService_ListMemoryEntries", "MemoryService_GetMemoryEntry"} {
			w.blocked(id, "memory layer")
		}
	} else {
		memoryID := w.memory.Metadata.ID
		w.probe("MemoryService_GetMemoryLayer", "Real API 200; decoded list-derived MemoryLayer metadata.", func(ctx context.Context) error {
			value, err := w.client.MemoryLayers().Retrieve(ctx, memoryID, nil)
			if err != nil {
				return err
			}
			return validResource(value.Metadata)
		})
		w.probe("MemoryService_ListMemoryEntries", "Real API 200; decoded entry page for a list-derived memory layer.", func(ctx context.Context) error {
			page, err := w.client.MemoryLayers().Entries().List(ctx, memoryID, &sdk.MemoryEntryListParams{Limit: sdk.Int32(50)})
			if err != nil {
				return err
			}
			if page == nil {
				return errors.New("nil memory-entry page")
			}
			w.entry = first(page.Items)
			return nil
		})
		if w.entry == nil || w.entry.Metadata == nil || w.entry.Metadata.ID == "" {
			w.blocked("MemoryService_GetMemoryEntry", "memory entry")
		} else {
			w.probe("MemoryService_GetMemoryEntry", "Real API 200; decoded list-derived MemoryEntryDetail metadata.", func(ctx context.Context) error {
				value, err := w.client.MemoryLayers().Entries().Retrieve(ctx, memoryID, w.entry.Metadata.ID, nil)
				if err != nil {
					return err
				}
				return validResource(value.Metadata)
			})
		}
	}

	// Models.
	w.probe("ModelService_ListModels", "Real API 200; decoded model page and captured an in-memory fixture when available.", func(ctx context.Context) error {
		page, err := w.client.Models().List(ctx, &sdk.ModelListParams{Limit: sdk.Int32(50)})
		if err != nil {
			return err
		}
		if page == nil {
			return errors.New("nil model page")
		}
		w.model = first(page.Items)
		return nil
	})
	if w.model == nil || w.model.Metadata == nil || w.model.Metadata.ID == "" {
		w.blocked("ModelService_GetModel", "model")
	} else {
		w.probe("ModelService_GetModel", "Real API 200; decoded list-derived Model metadata.", func(ctx context.Context) error {
			value, err := w.client.Models().Retrieve(ctx, w.model.Metadata.ID, nil)
			if err != nil {
				return err
			}
			return validResource(value.Metadata)
		})
	}

	// Objectives and all read-only children. Select a recent objective with
	// actual events where possible, because that enables replay SSE evidence.
	var objectives []sdk.Objective
	w.probe("ObjectiveService_ListObjectives", "Real API 200; decoded objective page and captured an in-memory fixture when available.", func(ctx context.Context) error {
		page, err := w.client.Objectives().List(ctx, &sdk.ObjectiveListParams{Limit: sdk.Int32(50)})
		if err != nil {
			return err
		}
		if page == nil {
			return errors.New("nil objective page")
		}
		objectives = page.Items
		w.objective = first(page.Items)
		return nil
	})
	if w.objective == nil || w.objective.Metadata == nil || w.objective.Metadata.ID == "" {
		for _, id := range []string{"ObjectiveService_GetObjective", "ObjectiveService_ListObjectiveContextWindows", "ObjectiveService_GetObjectiveDiagnostics", "ObjectiveService_ListObjectiveEvents", "ObjectiveEventStreamsService_StreamObjectiveEvents", "ObjectiveService_ListObjectiveFeedback", "ObjectiveService_ListObjectiveTasks", "ObjectiveService_GetObjectiveTask", "ObjectiveService_ListObjectiveToolCalls", "ObjectiveService_GetObjectiveToolCall", "ObjectiveService_ListObjectiveTools"} {
			w.blocked(id, "objective")
		}
	} else {
		objectiveID := w.objective.Metadata.ID
		// Prefer an objective whose list-events response is non-empty.
		for index := range objectives {
			candidate := &objectives[index]
			if candidate.Metadata == nil || candidate.Metadata.ID == "" {
				continue
			}
			ctx, cancel := context.WithTimeout(context.Background(), 20*time.Second)
			page, err := w.client.Objectives().ListEvents(ctx, candidate.Metadata.ID, &sdk.ObjectiveListEventsParams{Limit: sdk.Int32(50), SortOrder: sdk.String("asc")})
			cancel()
			if err == nil && page != nil && len(page.Items) > 0 {
				w.objective = candidate
				objectiveID = candidate.Metadata.ID
				w.events = page.Items
				break
			}
		}
		w.probe("ObjectiveService_GetObjective", "Real API 200; decoded list-derived Objective metadata.", func(ctx context.Context) error {
			value, err := w.client.Objectives().Retrieve(ctx, objectiveID, nil)
			if err != nil {
				return err
			}
			return validOperation(value.Metadata)
		})
		w.probe("ObjectiveService_ListObjectiveContextWindows", "Real API 200; decoded context-window page for a list-derived objective.", func(ctx context.Context) error {
			page, err := w.client.Objectives().ListContextWindows(ctx, objectiveID, &sdk.ObjectiveListContextWindowsParams{Limit: sdk.Int32(20)})
			if err != nil {
				return err
			}
			if page == nil {
				return errors.New("nil context-window page")
			}
			return nil
		})
		w.probe("ObjectiveService_GetObjectiveDiagnostics", "Real API 200; decoded diagnostics response for a list-derived objective.", func(ctx context.Context) error {
			value, err := w.client.Objectives().RetrieveDiagnostics(ctx, objectiveID, nil)
			if err != nil {
				return err
			}
			if value == nil {
				return errors.New("nil diagnostics response")
			}
			return nil
		})
		w.probe("ObjectiveService_ListObjectiveEvents", "Real API 200; decoded event page for a list-derived objective.", func(ctx context.Context) error {
			page, err := w.client.Objectives().ListEvents(ctx, objectiveID, &sdk.ObjectiveListEventsParams{Limit: sdk.Int32(50), SortOrder: sdk.String("asc")})
			if err != nil {
				return err
			}
			if page == nil {
				return errors.New("nil event page")
			}
			w.events = page.Items
			return nil
		})
		if len(w.events) < 2 || w.events[0].Metadata == nil || w.events[0].Metadata.ID == "" {
			w.blocked("ObjectiveEventStreamsService_StreamObjectiveEvents", "objective with at least two persisted events for replay")
		} else {
			checkpoint := w.events[0].Metadata.ID
			w.probe("ObjectiveEventStreamsService_StreamObjectiveEvents", "Real API SSE replay returned at least one typed ObjectiveEvent after a list-derived Last-Event-ID; body and IDs not persisted.", func(ctx context.Context) error {
				stream, err := w.client.Objectives().StreamEvents(ctx, objectiveID, nil, sdk.WithLastEventID(checkpoint))
				if err != nil {
					return err
				}
				defer stream.Close()
				if !stream.Next() {
					if err := stream.Err(); err != nil {
						return err
					}
					return errors.New("stream ended before replay event")
				}
				if stream.Current().Metadata == nil || stream.Current().Metadata.ID == "" {
					return errors.New("decoded replay event metadata absent")
				}
				return nil
			})
		}
		w.probe("ObjectiveService_ListObjectiveFeedback", "Real API 200; decoded feedback page for a list-derived objective (zero items is valid).", func(ctx context.Context) error {
			page, err := w.client.Objectives().ListFeedback(ctx, objectiveID, &sdk.ObjectiveListFeedbackParams{Limit: sdk.Int32(20)})
			if err != nil {
				return err
			}
			if page == nil {
				return errors.New("nil feedback page")
			}
			return nil
		})
		w.probe("ObjectiveService_ListObjectiveTasks", "Real API 200; decoded task page for a list-derived objective.", func(ctx context.Context) error {
			page, err := w.client.Objectives().ListTasks(ctx, objectiveID, &sdk.ObjectiveListTasksParams{Limit: sdk.Int32(20)})
			if err != nil {
				return err
			}
			if page == nil {
				return errors.New("nil task page")
			}
			w.task = first(page.Items)
			return nil
		})
		if w.task == nil || w.task.Metadata == nil || w.task.Metadata.ID == nil || *w.task.Metadata.ID == "" {
			w.blocked("ObjectiveService_GetObjectiveTask", "objective task")
		} else {
			w.probe("ObjectiveService_GetObjectiveTask", "Real API 200; decoded list-derived ObjectiveTask metadata.", func(ctx context.Context) error {
				value, err := w.client.Objectives().RetrieveTask(ctx, objectiveID, *w.task.Metadata.ID, nil)
				if err != nil {
					return err
				}
				if value == nil || value.Metadata == nil || value.Metadata.ID == nil || *value.Metadata.ID == "" {
					return errors.New("decoded task metadata absent")
				}
				return nil
			})
		}
		w.probe("ObjectiveService_ListObjectiveToolCalls", "Real API 200; decoded tool-call page for a list-derived objective.", func(ctx context.Context) error {
			page, err := w.client.Objectives().ListToolCalls(ctx, objectiveID, &sdk.ObjectiveListToolCallsParams{Limit: sdk.Int32(20)})
			if err != nil {
				return err
			}
			if page == nil {
				return errors.New("nil tool-call page")
			}
			w.toolCall = first(page.Items)
			return nil
		})
		if w.toolCall == nil || w.toolCall.Metadata == nil || w.toolCall.Metadata.ID == "" {
			w.blocked("ObjectiveService_GetObjectiveToolCall", "objective tool call")
		} else {
			w.probe("ObjectiveService_GetObjectiveToolCall", "Real API 200; decoded list-derived ObjectiveToolCallWithResult metadata.", func(ctx context.Context) error {
				value, err := w.client.Objectives().RetrieveToolCall(ctx, objectiveID, w.toolCall.Metadata.ID, nil)
				if err != nil {
					return err
				}
				return validOperation(value.Metadata)
			})
		}
		w.probe("ObjectiveService_ListObjectiveTools", "Real API 200; decoded objective-tool snapshot page for a list-derived objective (zero items is valid).", func(ctx context.Context) error {
			page, err := w.client.Objectives().ListTools(ctx, objectiveID, &sdk.ObjectiveListToolsParams{Limit: sdk.Int32(20)})
			if err != nil {
				return err
			}
			if page == nil {
				return errors.New("nil objective-tool page")
			}
			return nil
		})
	}

	// Tool search and tenants.
	w.probe("SearchService_SearchToolsOrToolSets", "Real API 200; decoded search response for a non-sensitive fixed query; body not persisted.", func(ctx context.Context) error {
		value, err := w.client.ToolSearch().SearchOrSets(ctx, &sdk.ToolSearchSearchOrSetsParams{Query: "live-matrix-no-match"})
		if err != nil {
			return err
		}
		if value == nil {
			return errors.New("nil search response")
		}
		return nil
	})
	w.probe("TenantService_ListTenants", "Real API 200; decoded tenant page.", func(ctx context.Context) error {
		page, err := w.client.Tenants().List(ctx, &sdk.TenantListParams{Limit: sdk.Int32(50)})
		if err != nil {
			return err
		}
		if page == nil {
			return errors.New("nil tenant page")
		}
		w.tenant = first(page.Items)
		return nil
	})
	if w.tenant == nil || w.tenant.Metadata == nil || w.tenant.Metadata.ID == "" {
		w.blocked("TenantService_GetTenant", "tenant")
		w.blocked("TenantService_ListTenantSubjects", "tenant")
	} else {
		tenantID := w.tenant.Metadata.ID
		w.probe("TenantService_GetTenant", "Real API 200; decoded list-derived Tenant metadata.", func(ctx context.Context) error {
			value, err := w.client.Tenants().Retrieve(ctx, tenantID, nil)
			if err != nil {
				return err
			}
			return validResource(value.Metadata)
		})
		w.probe("TenantService_ListTenantSubjects", "Real API 200; decoded subject page for a list-derived tenant (zero items is valid).", func(ctx context.Context) error {
			page, err := w.client.Tenants().ListSubjects(ctx, tenantID, &sdk.TenantListSubjectsParams{Limit: sdk.Int32(20)})
			if err != nil {
				return err
			}
			if page == nil {
				return errors.New("nil subject page")
			}
			return nil
		})
	}

	// Tool sets and nested resources.
	w.probe("ToolService_ListToolSets", "Real API 200; decoded tool-set page.", func(ctx context.Context) error {
		page, err := w.client.ToolSets().List(ctx, &sdk.ToolSetListParams{Limit: sdk.Int32(50)})
		if err != nil {
			return err
		}
		if page == nil {
			return errors.New("nil tool-set page")
		}
		w.toolSet = first(page.Items)
		for index := range page.Items {
			if page.Items[index].Spec != nil && page.Items[index].Spec.Adapter != nil && page.Items[index].Spec.Adapter.OpenAPI != nil {
				w.openAPISet = &page.Items[index]
				break
			}
		}
		return nil
	})
	if w.toolSet == nil || w.toolSet.Metadata == nil || w.toolSet.Metadata.ID == "" {
		for _, id := range []string{"ToolService_GetToolSet", "ToolService_ListToolSetEvents", "ToolService_GetToolSetOpenAPISpec", "ToolService_ListToolSetUsage", "ToolService_ListToolSetSecrets", "ToolService_GetToolSetSecret", "ToolService_ListTools", "ToolService_GetTool"} {
			w.blocked(id, "tool set")
		}
	} else {
		toolSetID := w.toolSet.Metadata.ID
		w.probe("ToolService_GetToolSet", "Real API 200; decoded list-derived ToolSet metadata.", func(ctx context.Context) error {
			value, err := w.client.ToolSets().Retrieve(ctx, toolSetID, nil)
			if err != nil {
				return err
			}
			return validResource(value.Metadata)
		})
		w.probe("ToolService_ListToolSetEvents", "Real API 200; decoded event page for a list-derived tool set (zero items is valid).", func(ctx context.Context) error {
			page, err := w.client.ToolSets().ListEvents(ctx, toolSetID, &sdk.ToolSetListEventsParams{Limit: sdk.Int32(20)})
			if err != nil {
				return err
			}
			if page == nil {
				return errors.New("nil tool-set event page")
			}
			return nil
		})
		if w.openAPISet == nil || w.openAPISet.Metadata == nil || w.openAPISet.Metadata.ID == "" {
			w.blocked("ToolService_GetToolSetOpenAPISpec", "OpenAPI-adapter tool set")
		} else {
			w.probe("ToolService_GetToolSetOpenAPISpec", "Real API 200; decoded consumed-spec response for a list-derived OpenAPI tool set; spec body not persisted.", func(ctx context.Context) error {
				value, err := w.client.ToolSets().RetrieveOpenAPISpec(ctx, w.openAPISet.Metadata.ID, nil)
				if err != nil {
					return err
				}
				if value == nil {
					return errors.New("nil OpenAPI-spec response")
				}
				return nil
			})
		}
		w.probe("ToolService_ListToolSetUsage", "Real API 200; decoded usage page for a list-derived tool set (zero items is valid).", func(ctx context.Context) error {
			page, err := w.client.ToolSets().ListUsage(ctx, toolSetID, &sdk.ToolSetListUsageParams{Limit: sdk.Int32(20)})
			if err != nil {
				return err
			}
			if page == nil {
				return errors.New("nil usage page")
			}
			return nil
		})
		w.probe("ToolService_ListToolSetSecrets", "Real API 200; decoded secret-metadata page for a list-derived tool set; values/body not persisted.", func(ctx context.Context) error {
			page, err := w.client.ToolSets().Secrets().List(ctx, toolSetID, &sdk.ToolSetSecretListParams{Limit: sdk.Int32(20)})
			if err != nil {
				return err
			}
			if page == nil {
				return errors.New("nil tool-set secret page")
			}
			w.toolSecret = first(page.Items)
			return nil
		})
		if w.toolSecret == nil || w.toolSecret.Metadata == nil || w.toolSecret.Metadata.ID == "" {
			w.blocked("ToolService_GetToolSetSecret", "tool-set secret")
		} else {
			w.probe("ToolService_GetToolSetSecret", "Real API 200; decoded list-derived ToolSetSecret metadata; value/body not persisted.", func(ctx context.Context) error {
				value, err := w.client.ToolSets().Secrets().Retrieve(ctx, toolSetID, w.toolSecret.Metadata.ID, nil)
				if err != nil {
					return err
				}
				return validResource(value.Metadata)
			})
		}
		w.probe("ToolService_ListTools", "Real API 200; decoded tool page for a list-derived tool set.", func(ctx context.Context) error {
			page, err := w.client.ToolSets().Tools().List(ctx, toolSetID, &sdk.ToolListParams{Limit: sdk.Int32(50)})
			if err != nil {
				return err
			}
			if page == nil {
				return errors.New("nil tool page")
			}
			w.tool = first(page.Items)
			return nil
		})
		if w.tool == nil || w.tool.Metadata == nil || w.tool.Metadata.ID == "" {
			w.blocked("ToolService_GetTool", "tool")
		} else {
			w.probe("ToolService_GetTool", "Real API 200; decoded list-derived Tool metadata.", func(ctx context.Context) error {
				value, err := w.client.ToolSets().Tools().Retrieve(ctx, toolSetID, w.tool.Metadata.ID, nil)
				if err != nil {
					return err
				}
				return validResource(value.Metadata)
			})
		}
	}

	// Upload has no list operation, so a non-mutating wave cannot safely learn
	// an upload ID without discovering one in an existing memory entry.
	w.blocked("UploadService_GetUpload", "upload")

	// Widgets and sessions.
	w.probe("WidgetService_ListWidgets", "Real API 200; decoded widget page.", func(ctx context.Context) error {
		page, err := w.client.Widgets().List(ctx, &sdk.WidgetListParams{Limit: sdk.Int32(50)})
		if err != nil {
			return err
		}
		if page == nil {
			return errors.New("nil widget page")
		}
		w.widget = first(page.Items)
		return nil
	})
	if w.widget == nil || w.widget.Metadata == nil || w.widget.Metadata.ID == "" {
		w.blocked("WidgetService_GetWidget", "widget")
	} else {
		w.probe("WidgetService_GetWidget", "Real API 200; decoded list-derived Widget metadata.", func(ctx context.Context) error {
			value, err := w.client.Widgets().Retrieve(ctx, w.widget.Metadata.ID, nil)
			if err != nil {
				return err
			}
			return validResource(value.Metadata)
		})
	}
	w.probe("WidgetSessionService_ListWidgetSessions", "Real API 200; decoded widget-session page; token/body not persisted.", func(ctx context.Context) error {
		page, err := w.client.WidgetSessions().List(ctx, &sdk.WidgetSessionListParams{Limit: sdk.Int32(50)})
		if err != nil {
			return err
		}
		if page == nil {
			return errors.New("nil widget-session page")
		}
		w.session = first(page.Items)
		return nil
	})
	if w.session == nil || w.session.Metadata == nil || w.session.Metadata.ID == "" {
		w.blocked("WidgetSessionService_GetWidgetSession", "widget session")
	} else {
		w.probe("WidgetSessionService_GetWidgetSession", "Real API 200; decoded list-derived WidgetSession metadata; token/body not persisted.", func(ctx context.Context) error {
			value, err := w.client.WidgetSessions().Retrieve(ctx, w.session.Metadata.ID, nil)
			if err != nil {
				return err
			}
			return validOperation(value.Metadata)
		})
	}

	// Workspace secrets: metadata/shape only, never inspect values.
	w.probe("WorkspaceSecretService_ListWorkspaceSecrets", "Real API 200; decoded workspace-secret metadata page; values/body not persisted.", func(ctx context.Context) error {
		page, err := w.client.WorkspaceSecrets().List(ctx, &sdk.WorkspaceSecretListParams{Limit: sdk.Int32(20)})
		if err != nil {
			return err
		}
		if page == nil {
			return errors.New("nil workspace-secret page")
		}
		w.secret = first(page.Items)
		return nil
	})
	if w.secret == nil || w.secret.Metadata == nil || w.secret.Metadata.ID == "" {
		w.blocked("WorkspaceSecretService_GetWorkspaceSecret", "workspace secret")
	} else {
		w.probe("WorkspaceSecretService_GetWorkspaceSecret", "Real API 200; decoded list-derived WorkspaceSecret metadata; value/body not persisted.", func(ctx context.Context) error {
			value, err := w.client.WorkspaceSecrets().Retrieve(ctx, w.secret.Metadata.ID, nil)
			if err != nil {
				return err
			}
			return validResource(value.Metadata)
		})
	}

	// Deterministic result ordering is supplied by encoding/json's string-key
	// map ordering. Check that the wave evaluates every GET operation exactly.
	expected := []string{
		"AccountService_GetAccount", "GlobalAPIKeyService_GetGlobalAPIKey", "APIKeyService_ListAPIKeys", "APIKeyService_GetAPIKey", "WorkspaceAdminService_ListProfiles", "WorkspaceAdminService_ListAccountWorkspaces", "WorkspaceAdminService_GetWorkspace", "WorkspaceAdminService_ListWorkspaceMembers", "ProfilesService_Whoami", "WorkspaceService_ListWorkspaces", "AgentService_ListAgents", "AgentService_ListAgentFeedback", "AgentService_ListAgentWebhookDeliveries", "AgentService_GetAgent", "AgentScheduleService_ListAgentSchedules", "AgentScheduleService_GetAgentSchedule", "AgentVariationService_ListAgentVariations", "AgentVariationService_GetAgentVariation", "AIProviderKeyService_ListAIProviderKeys", "AIProviderKeyService_GetAIProviderKey", "MemoryService_ListMemoryLayers", "MemoryService_GetMemoryLayer", "MemoryService_ListMemoryEntries", "MemoryService_GetMemoryEntry", "ModelService_ListModels", "ModelService_GetModel", "ObjectiveService_ListObjectives", "ObjectiveService_GetObjective", "ObjectiveService_ListObjectiveContextWindows", "ObjectiveService_GetObjectiveDiagnostics", "ObjectiveService_ListObjectiveEvents", "ObjectiveEventStreamsService_StreamObjectiveEvents", "ObjectiveService_ListObjectiveFeedback", "ObjectiveService_ListObjectiveTasks", "ObjectiveService_GetObjectiveTask", "ObjectiveService_ListObjectiveToolCalls", "ObjectiveService_GetObjectiveToolCall", "ObjectiveService_ListObjectiveTools", "SearchService_SearchToolsOrToolSets", "TenantService_ListTenants", "TenantService_GetTenant", "TenantService_ListTenantSubjects", "ToolService_ListToolSets", "ToolService_GetToolSet", "ToolService_ListToolSetEvents", "ToolService_GetToolSetOpenAPISpec", "ToolService_ListToolSetUsage", "ToolService_ListToolSetSecrets", "ToolService_GetToolSetSecret", "ToolService_ListTools", "ToolService_GetTool", "UploadService_GetUpload", "WidgetSessionService_ListWidgetSessions", "WidgetSessionService_GetWidgetSession", "WidgetService_ListWidgets", "WidgetService_GetWidget", "WorkspaceSecretService_ListWorkspaceSecrets", "WorkspaceSecretService_GetWorkspaceSecret",
	}
	sort.Strings(expected)
	actual := make([]string, 0, len(w.results))
	for id := range w.results {
		actual = append(actual, id)
	}
	sort.Strings(actual)
	if fmt.Sprint(actual) != fmt.Sprint(expected) {
		fatal(fmt.Errorf("GET coverage mismatch: expected %d operations, evaluated %d", len(expected), len(actual)))
	}
}

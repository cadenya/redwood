// Opt-in, owned-fixture acceptance run for adapter and objective state flows.
package main

import (
	"bufio"
	"context"
	"encoding/json"
	"errors"
	"fmt"
	"os"
	"path/filepath"
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

type cleanup struct {
	label string
	fn    func(context.Context) error
}

type wave struct {
	client          *sdk.Client
	run             string
	contentToolName string
	results         map[string]result
	cleanups        []cleanup
	agentID         string
	variationID     string
}

func main() {
	if os.Getenv("GO_LIVE_SPECIALIZED") != "1" {
		fatal(errors.New("set GO_LIVE_SPECIALIZED=1"))
	}
	envPath := os.Getenv("GO_LIVE_ENV_PATH")
	if envPath == "" {
		envPath = "../../../../.env.development"
	}
	if err := loadEnvOverride(envPath); err != nil {
		fatal(err)
	}
	client, err := sdk.NewClient()
	if err != nil {
		fatal(err)
	}
	nonce := fmt.Sprintf("%d", time.Now().UnixMilli())
	w := &wave{
		client:          client,
		run:             "specialized-go-" + nonce,
		contentToolName: "GoLiveContent_" + nonce,
		results:         readResults(),
	}
	failure := w.runAll()
	if failure != nil {
		fmt.Fprintln(os.Stderr, "specialized fixture failed:", safeError(failure))
	}
	for i := len(w.cleanups) - 1; i >= 0; i-- {
		ctx, cancel := context.WithTimeout(context.Background(), 30*time.Second)
		err := w.cleanups[i].fn(ctx)
		cancel()
		if err != nil {
			fmt.Fprintln(os.Stderr, "cleanup failed:", w.cleanups[i].label, safeError(err))
			if failure == nil {
				failure = fmt.Errorf("cleanup %s failed", w.cleanups[i].label)
			}
		}
	}
	writeResults(w.results)
	if failure != nil {
		os.Exit(1)
	}
	fmt.Println("Go specialized fixture acceptance PASSED")
}

func (w *wave) runAll() error {
	if err := w.petstore(); err != nil {
		w.fail("ToolService_GetToolSetOpenAPISpec", err)
		return err
	}
	fakerID, err := w.faker()
	if err != nil {
		return err
	}
	bareID, err := w.bareTool()
	if err != nil {
		return err
	}
	if err := w.agent(fakerID, bareID); err != nil {
		return err
	}
	approveID, checkpoint, err := w.approveFlow()
	if err != nil {
		return err
	}
	if err := w.replay(approveID, checkpoint); err != nil {
		w.fail("ObjectiveEventStreamsService_StreamObjectiveEvents", err)
		return err
	}
	if err := w.denyFlow(); err != nil {
		return err
	}
	if err := w.contentFlow(); err != nil {
		return err
	}
	return w.cancelFlow()
}

func (w *wave) petstore() error {
	openURL := sdk.NewToolSetAdapter_OpenAPIURL(sdk.ToolSetAdapter_OpenAPI_URL{
		URL:     "https://petstore3.swagger.io/api/v3/openapi.json",
		BaseURL: sdk.String("https://petstore3.swagger.io/api/v3"),
	})
	adapter := sdk.NewToolSetAdapterOpenAPI(sdk.ToolSetAdapter_OpenAPIVariant{OpenAPI: &openURL})
	id, err := w.createToolSet("petstore", adapter)
	if err != nil {
		return err
	}
	var parsed struct {
		OpenAPI string         `json:"openapi"`
		Paths   map[string]any `json:"paths"`
	}
	err = poll(2*time.Minute, func(ctx context.Context) (bool, error) {
		value, callErr := w.client.ToolSets().RetrieveOpenAPISpec(ctx, id, nil)
		if callErr != nil {
			return false, callErr
		}
		if value == nil || value.Spec == nil || json.Unmarshal([]byte(*value.Spec), &parsed) != nil {
			return false, nil
		}
		return parsed.OpenAPI != "" && len(parsed.Paths) >= 10, nil
	})
	if err == nil {
		w.complete("ToolService_GetToolSetOpenAPISpec", "Petstore URL adapter returned a decoded consumed OpenAPI document")
	}
	return err
}

func (w *wave) faker() (string, error) {
	matcher := sdk.NewToolSetAdapter_StringMatcherContains(sdk.ToolSetAdapter_StringMatcher_Contains{
		Contains: "Curse", CaseSensitive: sdk.Bool(false),
	})
	filter := &sdk.ToolSetAdapter_ToolFilter{
		Operator: sdk.ToolSetAdapterToolFilterOperatorOperatorAnd,
		Filters: []sdk.ToolSetAdapter_AttributeFilter{{
			Attribute: sdk.ToolSetAdapterAttributeFilterAttributeAttributeName,
			Matcher:   &matcher,
		}},
	}
	approval := sdk.NewToolSetAdapter_ApprovalRequirementFilterOnly(
		sdk.ToolSetAdapter_ApprovalRequirementFilter_Only{Only: filter},
	)
	adapter := sdk.NewToolSetAdapterMCP(sdk.ToolSetAdapter_MCPVariant{MCP: &sdk.ToolSetAdapter_MCP{
		URL: sdk.String("https://free.cadenya.com/faker-mcp"), ToolApprovals: &approval,
	}})
	id, err := w.createToolSet("faker-mcp", adapter)
	if err != nil {
		return "", err
	}
	err = poll(2*time.Minute, func(ctx context.Context) (bool, error) {
		page, callErr := w.client.ToolSets().Tools().List(ctx, id, &sdk.ToolListParams{Limit: sdk.Int32(20)})
		if callErr != nil {
			return false, callErr
		}
		got, approvals := map[string]bool{}, map[string]bool{}
		for _, tool := range page.Items {
			if tool.Spec != nil && tool.Spec.LlmToolName != nil {
				got[*tool.Spec.LlmToolName] = true
				approvals[*tool.Spec.LlmToolName] = tool.Spec.RequiresApproval
			}
		}
		return got["GenerateCurseWord"] && got["GenerateFake"] && got["GetFakerOptions"] &&
			approvals["GenerateCurseWord"] && !approvals["GenerateFake"] && !approvals["GetFakerOptions"], nil
	})
	return id, err
}

func (w *wave) bareTool() (string, error) {
	adapter := sdk.NewToolSetAdapterBare(sdk.ToolSetAdapter_BareVariant{Bare: &sdk.ToolSetAdapter_Bare{}})
	id, err := w.createToolSet("bare-content", adapter)
	if err != nil {
		return "", err
	}
	config := sdk.NewToolSpec_ConfigBare(sdk.ToolSpec_Config_Bare{Bare: &sdk.Config_Bare{}})
	tool, err := w.client.ToolSets().Tools().Create(context.Background(), id, &sdk.ToolCreateParams{
		Metadata: &sdk.CreateResourceMetadata{Name: w.run + "-content-tool", Labels: map[string]string{"live_matrix": w.run}},
		Spec: &sdk.ToolSpec{
			Description: "Returns externally supplied live-matrix content", RequiresApproval: false,
			Parameters: map[string]any{"type": "object", "properties": map[string]any{"value": map[string]any{"type": "string"}}, "required": []string{"value"}},
			Config:     &config, LlmToolName: &w.contentToolName,
		},
	})
	if err != nil {
		return "", err
	}
	if tool == nil || tool.Metadata == nil {
		return "", errors.New("bare tool metadata absent")
	}
	toolID := tool.Metadata.ID
	w.cleanups = append(w.cleanups, cleanup{"bare tool", func(ctx context.Context) error {
		return w.client.ToolSets().Tools().Delete(ctx, id, toolID, nil)
	}})
	return id, nil
}

func (w *wave) createToolSet(suffix string, adapter sdk.ToolSetAdapter) (string, error) {
	value, err := w.client.ToolSets().Create(context.Background(), &sdk.ToolSetCreateParams{
		Metadata: &sdk.CreateResourceMetadata{Name: w.run + "-" + suffix, Labels: map[string]string{"live_matrix": w.run}},
		Spec:     &sdk.ToolSetSpec{Description: sdk.String("Go specialized fixture"), Adapter: &adapter},
	})
	if err != nil {
		return "", err
	}
	if value == nil || value.Metadata == nil {
		return "", errors.New("tool set metadata absent")
	}
	id := value.Metadata.ID
	w.cleanups = append(w.cleanups, cleanup{"tool set " + suffix, func(ctx context.Context) error {
		_, _ = w.client.ToolSets().Archive(ctx, id, nil)
		return w.client.ToolSets().Delete(ctx, id, nil)
	}})
	return id, nil
}

func (w *wave) agent(toolSetIDs ...string) error {
	models, err := w.client.Models().List(context.Background(), &sdk.ModelListParams{Limit: sdk.Int32(50)})
	if err != nil {
		return err
	}
	var modelID string
	for _, model := range models.Items {
		if model.Metadata != nil && model.State == sdk.ModelStateStateEnabled {
			modelID = model.Metadata.ID
			break
		}
	}
	if modelID == "" {
		return errors.New("no enabled model")
	}
	value, err := w.client.Agents().Create(context.Background(), &sdk.AgentCreateParams{
		Metadata: &sdk.CreateResourceMetadata{Name: w.run + "-agent", Labels: map[string]string{"live_matrix": w.run}},
		Spec:     &sdk.AgentSpec{VariationSelectionMode: sdk.AgentSpecVariationSelectionModeVariationSelectionModeUnspecified},
		DefaultVariation: &sdk.CreateAgentVariationRequestParam{
			Metadata: &sdk.CreateResourceMetadata{Name: w.run + "-variation", Labels: map[string]string{"live_matrix": w.run}},
			Spec: &sdk.AgentVariationSpec{
				SystemPromptTemplate: sdk.String("You are an integration-test agent. Follow explicit tool-use instructions exactly."),
				ModelConfig:          &sdk.AgentVariationSpec_ModelConfig{ModelID: &modelID},
				Constraints:          &sdk.AgentVariationSpec_Constraints{MaxToolCalls: sdk.Int32(3), InactivityTimeout: sdk.String("300s")},
			},
		},
	})
	if err != nil {
		return err
	}
	if value == nil || value.Metadata == nil {
		return errors.New("agent metadata absent")
	}
	w.agentID = value.Metadata.ID
	w.cleanups = append(w.cleanups, cleanup{"agent", func(ctx context.Context) error {
		return w.client.Agents().Delete(ctx, w.agentID, nil)
	}})
	variations, err := w.client.Agents().Variations().List(context.Background(), w.agentID, &sdk.AgentVariationListParams{Limit: sdk.Int32(10)})
	if err != nil {
		return err
	}
	if len(variations.Items) == 0 || variations.Items[0].Metadata == nil {
		return errors.New("default variation absent")
	}
	w.variationID = variations.Items[0].Metadata.ID
	for _, toolSetID := range toolSetIDs {
		body := sdk.NewAddAgentVariationAssignmentRequestParamToolSetID(
			sdk.AddAgentVariationAssignmentRequest_ToolSetIDParam{ToolSetID: toolSetID},
		)
		assignment, callErr := w.client.Agents().Variations().AddAssignment(
			context.Background(), w.agentID, w.variationID,
			&sdk.AgentVariationAddAssignmentParams{Body: &body},
		)
		if callErr != nil {
			return callErr
		}
		assignmentID := variationAssignmentID(assignment)
		if assignmentID == "" {
			return errors.New("assignment id absent")
		}
		id := assignmentID
		w.cleanups = append(w.cleanups, cleanup{"assignment", func(ctx context.Context) error {
			return w.client.Agents().Variations().RemoveAssignment(ctx, w.agentID, w.variationID, id, nil)
		}})
	}
	_, err = w.client.Agents().Publish(context.Background(), w.agentID, nil)
	return err
}

func variationAssignmentID(value *sdk.VariationAssignment) string {
	if value == nil {
		return ""
	}
	if value.Tool != nil && value.Tool.ID != nil {
		return *value.Tool.ID
	}
	if value.ToolSet != nil && value.ToolSet.ID != nil {
		return *value.ToolSet.ID
	}
	if value.Agent != nil && value.Agent.ID != nil {
		return *value.Agent.ID
	}
	return ""
}

func (w *wave) createObjective(suffix, message string) (string, error) {
	value, err := w.client.Objectives().Create(context.Background(), &sdk.ObjectiveCreateParams{
		AgentID: w.agentID, VariationID: &w.variationID,
		Metadata:         &sdk.CreateOperationMetadata{Labels: map[string]string{"live_matrix": w.run, "case": suffix}},
		SystemPromptData: map[string]any{}, FirstUserMessage: sdk.String(message),
	})
	if err != nil {
		return "", err
	}
	if value == nil || value.Metadata == nil {
		return "", errors.New("objective metadata absent")
	}
	id := value.Metadata.ID
	w.cleanups = append(w.cleanups, cleanup{"objective " + suffix, func(ctx context.Context) error {
		current, retrieveErr := w.client.Objectives().Retrieve(ctx, id, nil)
		if retrieveErr != nil || current == nil {
			return retrieveErr
		}
		switch current.State {
		case sdk.ObjectiveStateStatePending, sdk.ObjectiveStateStateRunning, sdk.ObjectiveStateStateWaiting:
			_, cancelErr := w.client.Objectives().Cancel(ctx, id, &sdk.ObjectiveCancelParams{Reason: sdk.String("specialized fixture cleanup")})
			return cancelErr
		default:
			return nil
		}
	}})
	w.complete("ObjectiveService_CreateObjective", "created and dispatched an owned specialized objective")
	return id, nil
}

func (w *wave) approvalFixture(id string) (string, string, error) {
	ctx, cancel := context.WithTimeout(context.Background(), 2*time.Minute)
	defer cancel()
	stream, err := w.client.Objectives().StreamEvents(ctx, id, nil)
	if err != nil {
		return "", "", err
	}
	defer stream.Close()
	var checkpoint string
	for stream.Next() {
		event := stream.Current()
		if checkpoint == "" && event.Metadata != nil {
			checkpoint = event.Metadata.ID
		}
		if event.Data != nil && event.Data.ToolApprovalRequested != nil &&
			event.Data.ToolApprovalRequested.ToolApprovalRequested != nil &&
			event.Data.ToolApprovalRequested.ToolApprovalRequested.ToolCallID != nil {
			return *event.Data.ToolApprovalRequested.ToolApprovalRequested.ToolCallID, checkpoint, nil
		}
	}
	if stream.Err() != nil {
		return "", "", stream.Err()
	}
	return "", "", errors.New("stream ended before approval request")
}

func (w *wave) approveFlow() (string, string, error) {
	id, err := w.createObjective("approve", "Call GenerateCurseWord exactly once. Do not answer without using that tool.")
	if err != nil {
		return "", "", err
	}
	callID, checkpoint, err := w.approvalFixture(id)
	if err != nil {
		return "", "", err
	}
	w.complete("ObjectiveEventStreamsService_StreamObjectiveEvents", "SSE decoded a persisted approval event")
	if _, err = w.client.Objectives().ApproveToolCall(context.Background(), id, callID, nil); err != nil {
		w.fail("ObjectiveService_ApproveToolCall", err)
		return "", "", err
	}
	err = poll(2*time.Minute, func(ctx context.Context) (bool, error) {
		value, callErr := w.client.Objectives().RetrieveToolCall(ctx, id, callID, nil)
		return callErr == nil && value != nil && value.ExecutionStatus == sdk.ObjectiveToolCallWithResultExecutionStatusToolCallExecutionStatusCompleted, callErr
	})
	if err != nil {
		return "", "", err
	}
	w.complete("ObjectiveService_ApproveToolCall", "approved a Faker call and observed completed MCP execution")
	if err = waitState(id, w.client, sdk.ObjectiveStateStateWaiting, 2*time.Minute); err != nil {
		return "", "", err
	}
	score := float32(1)
	if _, err = w.client.Objectives().CreateFeedback(context.Background(), id, &sdk.ObjectiveCreateFeedbackParams{
		Metadata: &sdk.CreateOperationMetadata{Labels: map[string]string{"live_matrix": w.run}},
		Data:     &sdk.ObjectiveFeedbackData{Score: &score, Comment: sdk.String("Go specialized fixture")},
	}); err != nil {
		return "", "", err
	}
	w.complete("ObjectiveService_CreateObjectiveFeedback", "submitted feedback after completed MCP execution")
	instructions := "Summarize this integration-test conversation accurately."
	compacted, compactErr := w.client.Objectives().Compact(context.Background(), id, &sdk.ObjectiveCompactParams{
		CompactionConfig: &sdk.AgentVariationSpec_CompactionConfig{Summarization: &sdk.CompactionConfig_SummarizationStrategy{Instructions: &instructions}},
	})
	if compactErr != nil {
		w.fail("ObjectiveService_CompactObjective", compactErr)
		return "", "", compactErr
	}
	if compacted == nil {
		return "", "", errors.New("nil compact response")
	}
	w.complete("ObjectiveService_CompactObjective", "compacted an owned waiting objective after completed MCP execution")
	if _, err = w.client.Objectives().Continue(context.Background(), id, &sdk.ObjectiveContinueParams{
		Message: "Reply exactly CONTINUE_OK.", Enqueue: sdk.Bool(false),
	}); err != nil {
		return "", "", err
	}
	w.complete("ObjectiveService_ContinueObjective", "continued an owned waiting objective")
	return id, checkpoint, nil
}

func (w *wave) replay(id, checkpoint string) error {
	if checkpoint == "" {
		return errors.New("empty SSE checkpoint")
	}
	ctx, cancel := context.WithTimeout(context.Background(), 30*time.Second)
	defer cancel()
	stream, err := w.client.Objectives().StreamEvents(ctx, id, nil, sdk.WithLastEventID(checkpoint))
	if err != nil {
		return err
	}
	defer stream.Close()
	for stream.Next() {
		if stream.Current().Metadata != nil && stream.Current().Metadata.ID != "" && stream.Current().Metadata.ID != checkpoint {
			w.complete("ObjectiveEventStreamsService_StreamObjectiveEvents", "SSE Last-Event-ID replay decoded a later persisted event")
			return nil
		}
	}
	if stream.Err() != nil {
		return stream.Err()
	}
	return errors.New("replay returned no later event")
}

func (w *wave) denyFlow() error {
	id, err := w.createObjective("deny", "Call GenerateCurseWord exactly once. Do not answer without using that tool.")
	if err != nil {
		return err
	}
	callID, _, err := w.approvalFixture(id)
	if err != nil {
		return err
	}
	if _, err = w.client.Objectives().DenyToolCall(context.Background(), id, callID, &sdk.ObjectiveDenyToolCallParams{Memo: sdk.String("Use no replacement tool.")}); err != nil {
		w.fail("ObjectiveService_DenyToolCall", err)
		return err
	}
	err = poll(90*time.Second, func(ctx context.Context) (bool, error) {
		value, callErr := w.client.Objectives().RetrieveToolCall(ctx, id, callID, nil)
		return callErr == nil && value != nil && value.Status == sdk.ObjectiveToolCallWithResultStatusToolCallStatusDenied, callErr
	})
	if err == nil {
		w.complete("ObjectiveService_DenyToolCall", "denied an independent Faker call and observed persisted denial")
	}
	return err
}

func (w *wave) contentFlow() error {
	id, err := w.createObjective("content", "Call "+w.contentToolName+" exactly once with value live. Do not call any other tool.")
	if err != nil {
		return err
	}
	ctx, cancel := context.WithTimeout(context.Background(), 2*time.Minute)
	defer cancel()
	stream, err := w.client.Objectives().StreamEvents(ctx, id, nil)
	if err != nil {
		return err
	}
	defer stream.Close()
	var callID string
	for stream.Next() {
		event := stream.Current()
		if event.Data != nil && event.Data.ToolCalled != nil && event.Data.ToolCalled.ToolCalled != nil &&
			event.Data.ToolCalled.ToolCalled.ToolCallID != nil {
			callID = *event.Data.ToolCalled.ToolCalled.ToolCallID
			break
		}
	}
	if callID == "" {
		if stream.Err() != nil {
			return stream.Err()
		}
		return errors.New("stream ended before bare tool call")
	}
	content := sdk.NewSetToolCallContentRequest_ContentBlockText(
		sdk.SetToolCallContentRequest_ContentBlock_Text{Text: &sdk.SetToolCallContentRequest_TextBlock{Text: "content supplied"}},
	)
	if _, err = w.client.Objectives().SetToolCallContent(context.Background(), id, callID, &sdk.ObjectiveSetToolCallContentParams{Content: []sdk.SetToolCallContentRequest_ContentBlock{content}}); err != nil {
		w.fail("ObjectiveService_SetToolCallContent", err)
		return err
	}
	err = poll(90*time.Second, func(ctx context.Context) (bool, error) {
		value, callErr := w.client.Objectives().RetrieveToolCall(ctx, id, callID, nil)
		return callErr == nil && value != nil && value.ExecutionStatus == sdk.ObjectiveToolCallWithResultExecutionStatusToolCallExecutionStatusCompleted, callErr
	})
	if err == nil {
		w.complete("ObjectiveService_SetToolCallContent", "supplied text content to an independent bare tool call")
	}
	return err
}

func (w *wave) cancelFlow() error {
	id, err := w.createObjective("cancel", "Call GenerateFake once, then write a detailed response.")
	if err != nil {
		return err
	}
	err = poll(60*time.Second, func(ctx context.Context) (bool, error) {
		value, callErr := w.client.Objectives().Retrieve(ctx, id, nil)
		return callErr == nil && value != nil && value.State == sdk.ObjectiveStateStateRunning, callErr
	})
	if err != nil {
		return err
	}
	if _, err = w.client.Objectives().Cancel(context.Background(), id, &sdk.ObjectiveCancelParams{Reason: sdk.String("specialized running-cancel acceptance")}); err != nil {
		return err
	}
	err = waitState(id, w.client, sdk.ObjectiveStateStateCancelled, 60*time.Second)
	if err == nil {
		w.complete("ObjectiveService_CancelObjective", "cancelled a separate objective after observing RUNNING")
	}
	return err
}

func waitState(id string, client *sdk.Client, wanted sdk.ObjectiveState, timeout time.Duration) error {
	return poll(timeout, func(ctx context.Context) (bool, error) {
		value, err := client.Objectives().Retrieve(ctx, id, nil)
		return err == nil && value != nil && value.State == wanted, err
	})
}

func poll(timeout time.Duration, fn func(context.Context) (bool, error)) error {
	deadline := time.Now().Add(timeout)
	var last error
	for time.Now().Before(deadline) {
		ctx, cancel := context.WithTimeout(context.Background(), 30*time.Second)
		ok, err := fn(ctx)
		cancel()
		if ok {
			return nil
		}
		if err != nil {
			last = err
		}
		time.Sleep(250 * time.Millisecond)
	}
	if last != nil {
		return last
	}
	return errors.New("poll deadline exceeded")
}

func (w *wave) complete(id, evidence string) {
	w.results[id] = result{"completed", "real api.cadenya.com: Go specialized fixture succeeded; " + evidence}
}

func (w *wave) fail(id string, err error) {
	if w.results[id].Status != "completed" {
		w.results[id] = result{"failed", "real Go specialized fixture failed: " + safeError(err)}
	}
}

func (w *wave) block(id, evidence string) {
	if w.results[id].Status != "completed" {
		w.results[id] = result{"blocked", "real api.cadenya.com: Go specialized fixture; " + evidence}
	}
}

func safeError(err error) string {
	var apiErr *sdk.APIError
	if errors.As(err, &apiErr) {
		return fmt.Sprintf("API request failed (HTTP %d, code %d)", apiErr.StatusCode, apiErr.Code)
	}
	if errors.Is(err, context.DeadlineExceeded) {
		return "request deadline exceeded"
	}
	if errors.Is(err, context.Canceled) {
		return "request cancelled"
	}
	return fmt.Sprintf("request failed (%T)", err)
}

func fatal(err error) {
	fmt.Fprintln(os.Stderr, "Go specialized fixture:", safeError(err))
	os.Exit(1)
}

func resultPath() string { return filepath.Join("..", "..", "results-go.json") }

func readResults() map[string]result {
	raw, err := os.ReadFile(resultPath())
	if err != nil {
		return map[string]result{}
	}
	var current report
	if json.Unmarshal(raw, &current) != nil || current.Operations == nil {
		return map[string]result{}
	}
	return current.Operations
}

func writeResults(operations map[string]result) {
	raw, err := json.MarshalIndent(report{
		SchemaVersion: 1, SDK: "go", ExecutedAt: time.Now().UTC().Format(time.RFC3339), Operations: operations,
	}, "", "  ")
	if err != nil {
		fatal(err)
	}
	if err = os.WriteFile(resultPath(), append(raw, '\n'), 0o644); err != nil {
		fatal(err)
	}
}

func loadEnvOverride(path string) error {
	file, err := os.Open(path)
	if err != nil {
		return err
	}
	defer file.Close()
	scanner := bufio.NewScanner(file)
	for scanner.Scan() {
		line := strings.TrimSpace(scanner.Text())
		if line == "" || strings.HasPrefix(line, "#") {
			continue
		}
		line = strings.TrimPrefix(line, "export ")
		key, value, ok := strings.Cut(line, "=")
		if !ok {
			continue
		}
		key = strings.TrimSpace(key)
		if key != "CADENYA_API_KEY" && key != "CADENYA_WORKSPACE_ID" && key != "CADENYA_BASE_URL" {
			continue
		}
		value = strings.Trim(strings.TrimSpace(value), "\"'")
		if err = os.Setenv(key, value); err != nil {
			return err
		}
	}
	return scanner.Err()
}

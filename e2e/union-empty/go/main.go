// Decode-regression probe: optional discriminated unions must accept the
// protobuf-unset encodings (absent, null, {}, `"type": ""`) and still reject
// unknown non-empty discriminators. Run by e2e/union-empty.mjs.
package main

import (
	"encoding/json"
	"fmt"
	"os"
	"strings"

	sdk "go.cadenya.com/cadenya-go"
)

type wrapper struct {
	Config *sdk.AIProviderConfig `json:"config,omitempty"`
}

func main() {
	failures := 0
	check := func(label string, ok bool, detail string) {
		if ok {
			fmt.Printf("ok  go: %s\n", label)
		} else {
			failures++
			fmt.Printf("FAIL go: %s: %s\n", label, detail)
		}
	}
	noVariant := func(u *sdk.AIProviderConfig) bool {
		return u != nil && u.OpenRouter == nil && u.OpenAI == nil && u.OpenAICompatible == nil
	}

	for _, tc := range []struct {
		label string
		body  string
	}{
		{"config omitted", `{}`},
		{"config null", `{"config": null}`},
	} {
		var w wrapper
		if err := json.Unmarshal([]byte(tc.body), &w); err != nil {
			check(tc.label, false, err.Error())
		} else {
			check(tc.label, w.Config == nil || noVariant(w.Config), "unexpected variant set")
		}
	}

	for _, tc := range []struct {
		label string
		body  string
	}{
		{"config {} (protobuf empty)", `{"config": {}}`},
		{`config {"type": ""} (protobuf default)`, `{"config": {"type": ""}}`},
	} {
		var w wrapper
		if err := json.Unmarshal([]byte(tc.body), &w); err != nil {
			check(tc.label, false, err.Error())
		} else {
			check(tc.label, noVariant(w.Config), "expected zero-value union")
		}
	}

	for _, tc := range []struct {
		label string
		body  string
		probe func(u *sdk.AIProviderConfig) bool
	}{
		{"known tag openrouter", `{"config": {"type": "openrouter"}}`, func(u *sdk.AIProviderConfig) bool { return u.OpenRouter != nil }},
		{"known tag openai", `{"config": {"type": "openai"}}`, func(u *sdk.AIProviderConfig) bool { return u.OpenAI != nil }},
		{"known tag openaiCompatible", `{"config": {"type": "openaiCompatible"}}`, func(u *sdk.AIProviderConfig) bool { return u.OpenAICompatible != nil }},
	} {
		var w wrapper
		if err := json.Unmarshal([]byte(tc.body), &w); err != nil {
			check(tc.label, false, err.Error())
		} else {
			check(tc.label, w.Config != nil && tc.probe(w.Config), "variant not selected")
		}
	}

	var w wrapper
	err := json.Unmarshal([]byte(`{"config": {"type": "bogus"}}`), &w)
	check("unknown non-empty tag rejected", err != nil && strings.Contains(err.Error(), "unknown type"),
		fmt.Sprintf("err=%v", err))

	// Round-trip: a decoded zero-variant union must re-marshal (the CLI
	// re-encodes every response to print it) as the protobuf-unset {}.
	var rt wrapper
	if err := json.Unmarshal([]byte(`{"config": {}}`), &rt); err != nil {
		check("zero-variant union round-trips as {}", false, err.Error())
	} else if out, err := json.Marshal(rt); err != nil {
		check("zero-variant union round-trips as {}", false, err.Error())
	} else {
		check("zero-variant union round-trips as {}", strings.Contains(string(out), `"config":{}`), string(out))
	}

	// Endpoint-shaped fixture: a list page embedding a settings-less key.
	var page struct {
		Items []sdk.AIProviderKey `json:"items"`
	}
	fixture := `{"items": [{"metadata": {"id": "apk_1", "name": "anthropic"}, "spec": {"provider": "AI_PROVIDER_ANTHROPIC", "config": {}}}]}`
	if err := json.Unmarshal([]byte(fixture), &page); err != nil {
		check("ListAIProviderKeys-shaped page with empty config", false, err.Error())
	} else {
		check("ListAIProviderKeys-shaped page with empty config", len(page.Items) == 1, "item missing")
	}

	if failures > 0 {
		os.Exit(1)
	}
}

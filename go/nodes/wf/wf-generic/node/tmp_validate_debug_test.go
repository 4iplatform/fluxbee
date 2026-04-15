package node

import (
  "encoding/json"
  "testing"
)

func TestTmpValidateIntegerFromMapAny(t *testing.T) {
  def, err := LoadDefinitionBytes([]byte(validWorkflowJSON()), "", fixedClock)
  if err != nil { t.Fatal(err) }
  var input map[string]any
  if err := json.Unmarshal([]byte(`{"customer_id":"cust-001","amount_cents":25000,"currency":"USD"}`), &input); err != nil { t.Fatal(err) }
  if err := validateInputPayload(def, input); err != nil {
    t.Fatalf("validate err: %v (type=%T value=%v)", err, input["amount_cents"], input["amount_cents"])
  }
}

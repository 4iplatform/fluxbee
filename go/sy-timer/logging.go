//go:build linux

package main

import (
	"encoding/json"
	"log"
)

func logEvent(event string, fields map[string]any) {
	payload := map[string]any{
		"event": event,
	}
	for key, value := range fields {
		payload[key] = value
	}
	data, err := json.Marshal(payload)
	if err != nil {
		log.Printf("event=%s log_encoding_error=%q", event, err.Error())
		return
	}
	log.Printf("%s", data)
}

package sdk

import (
	"fmt"
	"log"
	"strings"
	"time"
)

// What the dispatcher does with a message a command channel's queue refused, its lane full (FINDINGS
// A-329): it answers the message's sender NOT_TAKEN, reason node_busy, retryable, unless the message
// is a notice or a reply, which no one answers (IsNoticeOrReply); it counts it (CommandChannelDrops,
// NotTakenSent); and its log says so, at once for the first, then at most a line every
// refusalLogEvery, which counts what came since. Retrying is the sender's: SY.timer sends a fire
// again (go/sy-timer), and a WF a timer request (go/nodes/wf/wf-generic); a node that does not act
// on a NOT_TAKEN (one before 0.1.141, an AI node, most IO nodes) loses the message, as it did
// before, but its sender's log now has the answer.

// refusalLogEvery is how often, at most, a command channel's log line says what its queue refused.
var refusalLogEvery = 10 * time.Second

// NotTakenCounts are what the dispatcher did with the messages its command channels refused:
// answered NOT_TAKEN (Answered), left unanswered, a notice or a reply (Unanswered), or answered with
// a NOT_TAKEN that could not go, the node's send queue full or its link down (SendFailed).
type NotTakenCounts struct {
	Answered   uint64
	Unanswered uint64
	SendFailed uint64
}

func (c NotTakenCounts) total() uint64 {
	return c.Answered + c.Unanswered + c.SendFailed
}

// refusalOutcome is what refuseCommand did with one refused message.
type refusalOutcome int

const (
	refusalAnswered refusalOutcome = iota
	refusalUnanswered
	refusalSendFailed
)

func (c *NotTakenCounts) add(outcome refusalOutcome) {
	switch outcome {
	case refusalAnswered:
		c.Answered++
	case refusalUnanswered:
		c.Unanswered++
	default:
		c.SendFailed++
	}
}

// refusalLog is a command channel's log of what its queue refused: when it last wrote a line, what
// came since, and whether a line is due (a timer writes what a burst left once refusalLogEvery
// passes).
type refusalLog struct {
	lastLine time.Time
	since    time.Time
	counts   NotTakenCounts
	depths   [laneCount]int
	last     string
	due      bool
}

// NotTakenSent returns what the dispatcher did so far with the messages its command channels
// refused (NotTakenCounts).
func (d *RouterDispatcher) NotTakenSent() NotTakenCounts {
	d.dropMu.Lock()
	defer d.dropMu.Unlock()
	return d.notTaken
}

// refuseCommand answers, counts and logs msg, which queue refused.
func (d *RouterDispatcher) refuseCommand(queue *commandQueue, msg Message) {
	lane := laneOf(msg.Meta.MsgType)
	depths := queue.depths()
	outcome := refusalUnanswered
	if !IsNoticeOrReply(&msg) && strings.TrimSpace(msg.Routing.Src) != "" {
		detail := fmt.Sprintf("%s holds %d %s messages, as many as it takes; send it again later",
			d.sender.FullName(), depths[lane], laneNames[lane])
		answer, err := BuildNotTaken(d.sender.UUID(), msg, NotTakenReasonNodeBusy, detail, true)
		if err == nil {
			err = d.sender.Send(answer)
		}
		if err == nil {
			outcome = refusalAnswered
		} else {
			outcome = refusalSendFailed
		}
	}
	now := time.Now()
	d.dropMu.Lock()
	d.commandDrops[queue.name]++
	d.notTaken.add(outcome)
	entry := d.refusalLogs[queue.name]
	if entry == nil {
		entry = &refusalLog{}
		d.refusalLogs[queue.name] = entry
	}
	if entry.counts.total() == 0 {
		entry.since = now
	}
	entry.counts.add(outcome)
	entry.depths = depths
	entry.last = fmt.Sprintf("msg_type=%q msg=%q from %q trace_id=%s",
		msg.Meta.MsgType, stringValue(msg.Meta.Msg), msg.SourceL2Name(), msg.Routing.TraceID)
	var line string
	switch wait := refusalLogEvery - now.Sub(entry.lastLine); {
	case entry.lastLine.IsZero() || wait <= 0:
		line = entry.line(queue.name, now)
	case !entry.due:
		entry.due = true
		time.AfterFunc(wait, func() { d.flushRefusalLog(queue.name) })
	}
	d.dropMu.Unlock()
	if line != "" {
		log.Print(line)
	}
}

// flushRefusalLog writes the line due for channel: what its queue refused since its last line.
func (d *RouterDispatcher) flushRefusalLog(channel string) {
	d.dropMu.Lock()
	entry := d.refusalLogs[channel]
	var line string
	if entry != nil {
		entry.due = false
		if entry.counts.total() > 0 {
			line = entry.line(channel, time.Now())
		}
	}
	d.dropMu.Unlock()
	if line != "" {
		log.Print(line)
	}
}

// line is the log line of what channel's queue refused since entry's last line, which it starts
// again at now. The caller holds dropMu.
func (entry *refusalLog) line(channel string, now time.Time) string {
	line := fmt.Sprintf(
		"fluxbee-go-sdk: command channel %q full (%d system and %d non-system messages waiting): "+
			"%d message(s) not taken since %s: %d answered NOT_TAKEN %s, %d not answered (a notice or a reply), "+
			"%d whose NOT_TAKEN could not be sent; the last: %s",
		channel, entry.depths[laneSystem], entry.depths[laneOther],
		entry.counts.total(), entry.since.UTC().Format("15:04:05.000Z"),
		entry.counts.Answered, NotTakenReasonNodeBusy, entry.counts.Unanswered, entry.counts.SendFailed,
		entry.last)
	entry.lastLine = now
	entry.counts = NotTakenCounts{}
	return line
}

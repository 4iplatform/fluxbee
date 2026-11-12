package sdk

import (
	"sync"
)

// A command channel's queue (FINDINGS A-329). The dispatcher hands each message a profile routes to
// a command channel to its queue, and the queue to the channel's consumer, the one that took it
// (TakeCommandReceiver). The queue has two lanes, system messages and every other kind, each
// bounded, and the consumer gets the oldest system message first, then the oldest of the rest: a
// system message (a NODE_STATUS_GET, a CONFIG_SET, a TIMER_FIRED) never waits behind user ones. A
// message that does not fit its lane is refused, and the dispatcher answers its sender NOT_TAKEN
// (RouterDispatcher.refuseCommand). Before 0.1.141 a command channel was one Go channel of 64
// messages, which dropped a message it could not hold and told no one.
//
// Each lane holds commandLaneMaxMessages messages or commandLaneMaxBytes bytes, whichever comes
// first: the bound of the router's own queue for a node's link (LINK_QUEUE_MAX_FRAMES and
// LINK_QUEUE_MAX_BYTES, src/router/link.rs, FINDINGS A-222), so a Go node holds what its link would.
// It takes the burst A-329 was seen with (1,000 messages at 50 a second to a WF that created about
// 30 conversations a second, about 400 waiting at its end) two and a half times over: about 34 s of
// that WF's work. Past it, the sender hears so and decides, rather than its message waiting longer
// with no one knowing.
const (
	commandLaneMaxMessages = 1024
	commandLaneMaxBytes    = 8 * 1024 * 1024
	// messageEnvelopeBytes is what approxMessageSize adds to a message's payload and context for the
	// rest of its frame, for a message whose frame size is not known (one a test hands the
	// dispatcher).
	messageEnvelopeBytes = 512
)

// The lanes of a command queue: system messages first.
const (
	laneSystem = iota
	laneOther
	laneCount
)

// laneNames are the lanes as a log line or a NOT_TAKEN's detail names them.
var laneNames = [laneCount]string{"system", "non-system"}

// commandQueue is a command channel's queue (above). Its forwarder (run) is the only one that takes
// a message out of a lane; offer, the dispatcher's, only adds at a lane's end.
type commandQueue struct {
	name string
	// out is the channel the consumer reads. It is unbuffered: the forwarder picks the next message
	// when the consumer takes one, so a system message that came meanwhile goes first.
	out chan Message
	// wake tells the forwarder a message came in (1 deep).
	wake chan struct{}

	mu          sync.Mutex
	lanes       [laneCount]commandLane
	maxMessages int
	maxBytes    int
}

// commandLane is one lane of a command queue: its messages, oldest first, and their bytes.
type commandLane struct {
	items []queuedMessage
	bytes int
}

type queuedMessage struct {
	msg  Message
	size int
}

func newCommandQueue(name string) *commandQueue {
	return &commandQueue{
		name:        name,
		out:         make(chan Message),
		wake:        make(chan struct{}, 1),
		maxMessages: commandLaneMaxMessages,
		maxBytes:    commandLaneMaxBytes,
	}
}

// laneOf is the lane of a message of kind msgType.
func laneOf(msgType string) int {
	if IsSystemKind(msgType) {
		return laneSystem
	}
	return laneOther
}

// offer queues msg, whose frame is size bytes, at the end of its lane, and reports whether it fit. A
// message never waits for room: one that does not fit is refused at once.
func (q *commandQueue) offer(msg Message, size int) bool {
	lane := laneOf(msg.Meta.MsgType)
	q.mu.Lock()
	l := &q.lanes[lane]
	if len(l.items) >= q.maxMessages || (len(l.items) > 0 && l.bytes+size > q.maxBytes) {
		q.mu.Unlock()
		return false
	}
	l.items = append(l.items, queuedMessage{msg: msg, size: size})
	l.bytes += size
	q.mu.Unlock()
	select {
	case q.wake <- struct{}{}:
	default:
	}
	return true
}

// depths are how many messages wait in each lane, the one the forwarder holds out to the consumer
// included.
func (q *commandQueue) depths() [laneCount]int {
	q.mu.Lock()
	defer q.mu.Unlock()
	var out [laneCount]int
	for lane := range q.lanes {
		out[lane] = len(q.lanes[lane].items)
	}
	return out
}

// head is the message the consumer gets next, and its lane: the oldest system message, else the
// oldest of the rest. False when both lanes are empty.
func (q *commandQueue) head() (Message, int, bool) {
	q.mu.Lock()
	defer q.mu.Unlock()
	for lane := range q.lanes {
		if items := q.lanes[lane].items; len(items) > 0 {
			return items[0].msg, lane, true
		}
	}
	return Message{}, 0, false
}

// pop takes the head of lane out: the message the consumer just took. Only the forwarder takes
// messages out, so the head of lane is still the one it handed over.
func (q *commandQueue) pop(lane int) {
	q.mu.Lock()
	defer q.mu.Unlock()
	l := &q.lanes[lane]
	l.bytes -= l.items[0].size
	l.items[0] = queuedMessage{}
	l.items = l.items[1:]
	if len(l.items) == 0 {
		l.items = nil
		l.bytes = 0
	}
}

// run is the queue's forwarder, until done: it holds out to the consumer the message head says, and
// takes it out of its lane once the consumer takes it. A message that comes in meanwhile makes it
// look again, so a system message goes before the user message it held out.
func (q *commandQueue) run(done <-chan struct{}) {
	for {
		msg, lane, ok := q.head()
		if !ok {
			select {
			case <-q.wake:
				continue
			case <-done:
				return
			}
		}
		select {
		case q.out <- msg:
			q.pop(lane)
		case <-q.wake:
		case <-done:
			return
		}
	}
}

// approxMessageSize is msg's frame size as its payload and context make it, for a message whose
// frame size is not known: what the lanes' byte bound counts.
func approxMessageSize(msg Message) int {
	return len(msg.Payload) + len(msg.Meta.Context) + messageEnvelopeBytes
}

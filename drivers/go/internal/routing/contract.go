// Package routing translates the canonical reddb-wire topology and SQL policy.
package routing

import (
	"encoding/binary"
	"errors"
	"net/url"
	"strconv"
	"strings"
	"unicode/utf8"
)

const TopologyKind byte = 0x32
const TopologyFeature uint32 = 2
const MetadataKey = "x-reddb-topology-transport"

type Endpoint struct {
	Addr   string `json:"addr"`
	Region string `json:"region"`
}
type Replica struct {
	Endpoint
	Healthy         bool   `json:"healthy"`
	LagMS           uint32 `json:"lag_ms"`
	LastAppliedLSN  string `json:"last_applied_lsn"`
	Rebootstrapping bool   `json:"rebootstrapping"`
}
type Topology struct {
	Version  int       `json:"version"`
	Epoch    string    `json:"epoch"`
	Primary  Endpoint  `json:"primary"`
	Replicas []Replica `json:"replicas"`
}

func Decode(data []byte) (*Topology, error) {
	malformed := errors.New("invalid topology payload")
	if len(data) < 5 {
		return nil, malformed
	}
	length := uint64(binary.LittleEndian.Uint32(data[1:5]))
	if length > uint64(len(data)-5) {
		return nil, malformed
	}
	if data[0] != 1 {
		return nil, nil
	}
	data = data[5 : 5+length]
	offset := 0
	take := func(size int) []byte {
		if size < 0 || size > len(data)-offset {
			return nil
		}
		value := data[offset : offset+size]
		offset += size
		return value
	}
	failed := false
	integer := func(size int) uint64 {
		value := take(size)
		if value == nil {
			failed = true
			return 0
		}
		switch size {
		case 8:
			return binary.LittleEndian.Uint64(value)
		case 4:
			return uint64(binary.LittleEndian.Uint32(value))
		default:
			return uint64(value[0])
		}
	}
	text := func() string {
		lengthBytes := take(4)
		if lengthBytes == nil {
			failed = true
			return ""
		}
		size := uint64(binary.LittleEndian.Uint32(lengthBytes))
		if size > 4096 {
			failed = true
			return ""
		}
		value := take(int(size))
		if value == nil || !utf8.Valid(value) {
			failed = true
			return ""
		}
		return string(value)
	}
	topology := &Topology{Version: 1, Epoch: strconv.FormatUint(integer(8), 10)}
	topology.Primary = Endpoint{Addr: text(), Region: text()}
	count := integer(4)
	if count > 256 {
		return nil, malformed
	}
	topology.Replicas = make([]Replica, 0, int(count))
	for index := uint64(0); index < count; index++ {
		topology.Replicas = append(topology.Replicas, Replica{
			Endpoint: Endpoint{Addr: text(), Region: text()}, Healthy: integer(1) != 0, LagMS: uint32(integer(4)),
			LastAppliedLSN: strconv.FormatUint(integer(8), 10),
		})
	}
	if uint64(len(data)-offset) >= count {
		for index := range topology.Replicas {
			topology.Replicas[index].Rebootstrapping = integer(1) != 0
		}
	}
	if failed {
		return nil, malformed
	}
	return topology, nil
}

func Address(address, protocol string) string {
	if address == "" || len(address) > 4096 {
		return ""
	}
	if !strings.Contains(address, "://") {
		address = protocol + "://" + address
	}
	parsed, err := url.Parse(address)
	if err != nil || parsed.Scheme != protocol || parsed.Hostname() == "" || parsed.User != nil || parsed.RawQuery != "" || parsed.Fragment != "" {
		return ""
	}
	return strings.TrimRight(parsed.String(), "/")
}

func Tokens(sql string) []string {
	if len(sql) > 1_048_576 {
		return nil
	}
	tokens := []string{}
	for index := 0; index < len(sql); {
		char := sql[index]
		switch {
		case char == ' ' || char == '\t' || char == '\n' || char == '\r':
			index++
		case strings.HasPrefix(sql[index:], "--"):
			end := strings.IndexByte(sql[index+2:], '\n')
			if end < 0 {
				index = len(sql)
			} else {
				index += end + 3
			}
		case strings.HasPrefix(sql[index:], "/*"):
			depth := 1
			index += 2
			for index < len(sql) && depth > 0 {
				if strings.HasPrefix(sql[index:], "/*") {
					depth++
					index += 2
				} else if strings.HasPrefix(sql[index:], "*/") {
					depth--
					index += 2
				} else {
					index++
				}
				if depth > 64 {
					return nil
				}
			}
			if depth != 0 {
				return nil
			}
		case char == '\'' || char == '"' || char == 96:
			index++
			closed := false
			for index < len(sql) {
				value := sql[index]
				index++
				if value == '\\' {
					return nil
				}
				if value == char {
					if index < len(sql) && sql[index] == char {
						index++
						continue
					}
					closed = true
					break
				}
			}
			if !closed {
				return nil
			}
			tokens = append(tokens, "#value")
		default:
			start := index
			index++
			if letter(char) {
				for index < len(sql) && (letter(sql[index]) || sql[index] >= '0' && sql[index] <= '9') {
					index++
				}
			}
			tokens = append(tokens, strings.ToUpper(sql[start:index]))
		}
	}
	if len(tokens) > 0 && tokens[len(tokens)-1] == ";" {
		tokens = tokens[:len(tokens)-1]
	}
	return tokens
}
func letter(value byte) bool {
	return value >= 'a' && value <= 'z' || value >= 'A' && value <= 'Z' || value == '_'
}
func contains(tokens []string, word string) bool {
	for _, token := range tokens {
		if token == word {
			return true
		}
	}
	return false
}

func SQLPolicy(sql string) (string, bool) {
	tokens := Tokens(sql)
	if len(tokens) == 0 || contains(tokens, ";") {
		return "session", false
	}
	first := tokens[0]
	if first == "BEGIN" || first == "START" && len(tokens) > 1 && tokens[1] == "TRANSACTION" {
		return "begin", false
	}
	if first == "COMMIT" || first == "END" || first == "ROLLBACK" {
		if first == "ROLLBACK" && contains(tokens, "TO") {
			return "primary", false
		}
		if contains(tokens, "CHAIN") && !contains(tokens, "NO") {
			return "begin", false
		}
		return "end", false
	}
	for _, word := range strings.Fields("SET RESET USE PREPARE DEALLOCATE DECLARE LISTEN") {
		if first == word {
			return "session", false
		}
	}
	read := first == "SELECT"
	for _, word := range strings.Fields("INSERT UPDATE DELETE CREATE ALTER DROP INTO FOR LOCK CALL EXECUTE SET QUEUE COPY") {
		if contains(tokens, word) {
			read = false
		}
	}
	functions := strings.Fields("COUNT SUM MIN MAX AVG LOWER UPPER LENGTH COALESCE ABS ROUND CAST EXISTS IN AS FROM WHERE AND OR NOT SELECT")
	for index, token := range tokens {
		if index+1 < len(tokens) && tokens[index+1] == "(" && letter(token[0]) && !contains(functions, token) {
			read = false
		}
	}
	return "primary", read
}

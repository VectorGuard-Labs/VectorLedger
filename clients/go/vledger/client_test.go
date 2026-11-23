package vledger

import (
	"errors"
	"bufio"
	"encoding/json"
	"fmt"
	"net"
	"testing"
	"time"
)

// ── Helpers ───────────────────────────────────────────────────────────────────

// startMockServer creates a local TCP server that replies with resp for every
// request.  Returns the address and a stop function.
func startMockServer(t *testing.T, replies []string) string {
	t.Helper()
	ln, err := net.Listen("tcp", "127.0.0.1:0")
	if err != nil {
		t.Fatalf("listen: %v", err)
	}
	t.Cleanup(func() { _ = ln.Close() })

	go func() {
		idx := 0
		for {
			conn, err := ln.Accept()
			if err != nil {
				return // listener closed
			}
			go func(c net.Conn) {
				defer c.Close()
				scanner := bufio.NewScanner(c)
				for scanner.Scan() {
					if idx >= len(replies) {
						idx = len(replies) - 1
					}
					fmt.Fprintln(c, replies[idx])
					idx++
				}
			}(conn)
		}
	}()

	return ln.Addr().String()
}

func okReply(columns []string, rows [][]any, msg string) string {
	resp := rawResponse{
		OK:           true,
		Columns:      columns,
		Rows:         rows,
		RowsAffected: len(rows),
		Message:      msg,
	}
	b, _ := json.Marshal(resp)
	return string(b)
}

func errReply(msg string) string {
	resp := rawResponse{OK: false, Error: msg}
	b, _ := json.Marshal(resp)
	return string(b)
}

func connectMock(t *testing.T, addr string) *Client {
	t.Helper()
	host, portStr, _ := net.SplitHostPort(addr)
	port := 0
	fmt.Sscanf(portStr, "%d", &port)

	c := &Client{
		opts: Options{
			Host:    host,
			Port:    port,
			UseTLS:  BoolPtr(false),
			Timeout: 5 * time.Second,
		},
	}
	if err := c.dial(); err != nil {
		t.Fatalf("dial: %v", err)
	}
	t.Cleanup(c.Close)
	return c
}

// ── Row tests ─────────────────────────────────────────────────────────────────

func TestRow_Get(t *testing.T) {
	row := Row{Columns: []string{"a", "b"}, Values: []any{1.0, "hello"}}
	v, err := row.Get("a")
	if err != nil || v != 1.0 {
		t.Fatalf("expected 1.0, got %v (err %v)", v, err)
	}
}

func TestRow_GetMissing(t *testing.T) {
	row := Row{Columns: []string{"x"}, Values: []any{"foo"}}
	_, err := row.Get("z")
	if err == nil {
		t.Fatal("expected error for missing column")
	}
}

func TestRow_GetOrDefault(t *testing.T) {
	row := Row{Columns: []string{}, Values: []any{}}
	v := row.GetOrDefault("missing", 42)
	if v != 42 {
		t.Fatalf("expected 42, got %v", v)
	}
}

func TestRow_ToMap(t *testing.T) {
	row := Row{Columns: []string{"id", "name"}, Values: []any{1.0, "Cash"}}
	m := row.ToMap()
	if m["name"] != "Cash" {
		t.Fatalf("unexpected map: %v", m)
	}
}

// ── Client tests ──────────────────────────────────────────────────────────────

func TestClient_Query(t *testing.T) {
	reply := okReply([]string{"balance"}, [][]any{{99000.0}}, "balance = 99000")
	addr := startMockServer(t, []string{reply})
	c := connectMock(t, addr)

	result, err := c.Query("SELECT BALANCE('CASH')")
	if err != nil {
		t.Fatalf("unexpected error: %v", err)
	}
	if len(result.Rows) != 1 {
		t.Fatalf("expected 1 row, got %d", len(result.Rows))
	}
	bal, _ := result.Rows[0].Get("balance")
	if bal != 99000.0 {
		t.Fatalf("expected 99000, got %v", bal)
	}
}

func TestClient_Execute(t *testing.T) {
	// RowsAffected is 0 when rows is empty, but we set it in the reply explicitly
	resp := rawResponse{OK: true, RowsAffected: 1, Message: "1 entry posted"}
	b, _ := json.Marshal(resp)

	addr := startMockServer(t, []string{string(b)})
	c := connectMock(t, addr)

	n, err := c.Execute("INSERT INTO ledger (...) VALUES (...)")
	if err != nil {
		t.Fatalf("unexpected error: %v", err)
	}
	if n != 1 {
		t.Fatalf("expected rows_affected=1, got %d", n)
	}
}

func TestClient_ErrorResponse(t *testing.T) {
	addr := startMockServer(t, []string{errReply("account 'X' not found")})
	c := connectMock(t, addr)

	_, err := c.Query("SELECT BALANCE('X')")
	if err == nil {
		t.Fatal("expected error, got nil")
	}
	vErr, ok := err.(*VledgerError)
	if !ok {
		t.Fatalf("expected *VledgerError, got %T", err)
	}
	if vErr.Message != "account 'X' not found" {
		t.Fatalf("unexpected message: %q", vErr.Message)
	}
}

func TestClient_VerifyChain(t *testing.T) {
	reply := okReply(
		[]string{"status", "entries_verified", "chain_tip"},
		[][]any{{"OK", 5.0, "abcdef"}},
		"verified",
	)
	addr := startMockServer(t, []string{reply})
	c := connectMock(t, addr)

	ok, err := c.VerifyChain()
	if err != nil || !ok {
		t.Fatalf("VerifyChain failed: %v", err)
	}
}

func TestParseQueryResult_WithProof(t *testing.T) {
	raw := &rawResponse{
		OK:      true,
		Columns: []string{"seq"},
		Rows:    [][]any{{1.0}},
		Proof:   &rawProof{RootHex: "deadbeef", LeafCount: 1, Verified: true},
	}
	result := parseQueryResult(raw)
	if result.Proof == nil {
		t.Fatal("expected proof")
	}
	if result.Proof.RootHex != "deadbeef" {
		t.Fatalf("unexpected root hex: %q", result.Proof.RootHex)
	}
}

// ── Authentication ────────────────────────────────────────────────────────────

// authServer enforces the real handshake: auth frame first, then token on every
// request. Frames received are recorded for assertions.
func authServer(t *testing.T) (addr string, frames *[]request) {
	t.Helper()
	ln, err := net.Listen("tcp", "127.0.0.1:0")
	if err != nil {
		t.Fatalf("listen: %v", err)
	}
	t.Cleanup(func() { _ = ln.Close() })
	var got []request
	go func() {
		for {
			conn, err := ln.Accept()
			if err != nil {
				return
			}
			go func(c net.Conn) {
				defer c.Close()
				sc := bufio.NewScanner(c)
				authed := false
				for sc.Scan() {
					var req request
					_ = json.Unmarshal(sc.Bytes(), &req)
					got = append(got, req)
					if !authed {
						if req.Auth != nil && req.Auth.Username == "alice" && req.Auth.Password == "s3cr3t" {
							authed = true
							fmt.Fprintln(c, `{"ok":true,"token":"tok123","role":"auditor","message":"authenticated"}`)
							continue
						}
						fmt.Fprintln(c, errReply("Authentication error: invalid credentials"))
						return // real server closes after a failed login
					}
					if req.Token != "tok123" {
						fmt.Fprintln(c, errReply("invalid or expired session token"))
						return
					}
					fmt.Fprintln(c, okReply([]string{"n"}, [][]any{{1.0}}, "ok"))
				}
			}(conn)
		}
	}()
	host, portStr, _ := net.SplitHostPort(ln.Addr().String())
	_ = host
	return "127.0.0.1:" + portStr, &got
}

func authOpts(addr, user, pass string) Options {
	_, portStr, _ := net.SplitHostPort(addr)
	port := 0
	fmt.Sscanf(portStr, "%d", &port)
	return Options{Host: "127.0.0.1", Port: port, UseTLS: BoolPtr(false), Username: user, Password: pass}
}

func TestAuth_LoginThenQueryCarriesToken(t *testing.T) {
	addr, frames := authServer(t)
	c, err := Connect(authOpts(addr, "alice", "s3cr3t"))
	if err != nil {
		t.Fatalf("connect: %v", err)
	}
	defer c.Close()
	if _, err := c.Query("SELECT 1"); err != nil {
		t.Fatalf("query: %v", err)
	}
	if len(*frames) < 2 || (*frames)[0].Auth == nil || (*frames)[0].Auth.Username != "alice" {
		t.Fatalf("first frame must be auth, got %+v", *frames)
	}
	if (*frames)[1].Token != "tok123" || (*frames)[1].SQL != "SELECT 1" {
		t.Fatalf("second frame must carry token and sql, got %+v", (*frames)[1])
	}
}

func TestAuth_WrongPassword(t *testing.T) {
	addr, _ := authServer(t)
	_, err := Connect(authOpts(addr, "alice", "nope"))
	var ae *VledgerAuthError
	if !errors.As(err, &ae) {
		t.Fatalf("expected VledgerAuthError, got %v", err)
	}
}

func TestAuth_NoCredentialsRejectedByServer(t *testing.T) {
	addr, _ := authServer(t)
	c, err := Connect(authOpts(addr, "", ""))
	if err != nil {
		t.Fatalf("connect: %v", err)
	}
	defer c.Close()
	if _, err := c.Query("SELECT 1"); err == nil {
		t.Fatal("expected the server to refuse an unauthenticated query")
	}
}

func TestAuth_UsernameWithoutPassword(t *testing.T) {
	if _, err := Connect(Options{Username: "alice", UseTLS: BoolPtr(false)}); err == nil {
		t.Fatal("expected an error when only Username is set")
	}
}

func TestAuth_ReconnectLogsInAgain(t *testing.T) {
	addr, frames := authServer(t)
	c, err := Connect(authOpts(addr, "alice", "s3cr3t"))
	if err != nil {
		t.Fatalf("connect: %v", err)
	}
	defer c.Close()
	if err := c.Reconnect(); err != nil {
		t.Fatalf("reconnect: %v", err)
	}
	if _, err := c.Query("SELECT 1"); err != nil {
		t.Fatalf("query after reconnect: %v", err)
	}
	n := 0
	for _, f := range *frames {
		if f.Auth != nil {
			n++
		}
	}
	if n != 2 {
		t.Fatalf("expected 2 auth frames, got %d", n)
	}
}

func TestClient_BalanceStringCell(t *testing.T) {
	// The real server serialises every cell as a string.
	reply := okReply([]string{"account", "balance"}, [][]any{{"CASH", "12345"}}, "balance = 12345")
	c := connectMock(t, startMockServer(t, []string{reply}))
	bal, err := c.Balance("CASH")
	if err != nil || bal != 12345 {
		t.Fatalf("expected 12345, got %d (err %v)", bal, err)
	}
}

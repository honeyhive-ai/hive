// Command gen prints the hive-tunnel/1 golden test vectors (docs/relay-tunnel.md).
// Every key, nonce and ephemeral is derived from a fixed label, so the output is
// fully deterministic. Run: go run . > vectors.json
package main

import (
	"bytes"
	"crypto/ed25519"
	"crypto/hkdf"
	"crypto/sha256"
	"encoding/base64"
	"encoding/binary"
	"encoding/hex"
	"encoding/json"
	"fmt"
	"os"
	"strings"

	"github.com/flynn/noise"
	"golang.org/x/crypto/chacha20poly1305"
	"golang.org/x/crypto/curve25519"
)

func seed(label string) []byte { h := sha256.Sum256([]byte("hive-tunnel test " + label)); return h[:] }
func b64(b []byte) string      { return base64.RawURLEncoding.EncodeToString(b) }
func hx(b []byte) string       { return hex.EncodeToString(b) }

func ctx(label string, parts ...[]byte) []byte {
	out := append([]byte("hive-tunnel/1 "+label), 0x00)
	for _, p := range parts {
		out = append(out, p...)
	}
	return out
}

func hk(ikm []byte, info string, n int) []byte {
	out, err := hkdf.Key(sha256.New, ikm, []byte("hive-tunnel/1"), info, n)
	must(err)
	return out
}

func x25519(priv []byte) noise.DHKey {
	pub, err := curve25519.X25519(priv, curve25519.Basepoint)
	must(err)
	return noise.DHKey{Private: priv, Public: pub}
}

func must(err error) {
	if err != nil {
		panic(err)
	}
}

// frame encodes one mux frame: type u8 | stream u32 BE | payload.
func frame(typ byte, stream uint32, payload []byte) []byte {
	out := make([]byte, 5, 5+len(payload))
	out[0] = typ
	binary.BigEndian.PutUint32(out[1:], stream)
	return append(out, payload...)
}

type msg struct {
	Dir       string `json:"dir"`
	Plaintext string `json:"plaintextHex"`
	Note      string `json:"note,omitempty"`
	Wire      string `json:"wireHex"`
}

type transcript struct {
	Protocol      string `json:"protocol"`
	PrologueHex   string `json:"prologueHex"`
	PskHex        string `json:"pskHex,omitempty"`
	PhoneEphPriv  string `json:"phoneEphemeralPrivHex"`
	HostEphPriv   string `json:"hostEphemeralPrivHex"`
	Msg1Payload   string `json:"msg1Payload"`
	Msg1Hex       string `json:"msg1Hex"`
	Msg2Payload   string `json:"msg2Payload"`
	Msg2Hex       string `json:"msg2Hex"`
	HandshakeHash string `json:"handshakeHashHex"`
	Transport     []msg  `json:"transport"`
	PushKeyHex    string `json:"pushKeyHex,omitempty"`
}

var suite = noise.NewCipherSuite(noise.DH25519, noise.CipherChaChaPoly, noise.HashSHA256)

// run performs the handshake; flynn/noise returns (cs1, cs2) on both sides, where
// cs1 encrypts initiator->responder and cs2 responder->initiator.
func run(prologue, psk []byte, phoneS, hostS, phoneE, hostE noise.DHKey, p1, p2 string, frames []struct {
	fromPhone bool
	pt        []byte
	note      string
}) transcript {
	ci := noise.Config{CipherSuite: suite, Pattern: noise.HandshakeIK, Initiator: true, Prologue: prologue,
		StaticKeypair: phoneS, Random: bytes.NewReader(phoneE.Private), PeerStatic: hostS.Public}
	cr := noise.Config{CipherSuite: suite, Pattern: noise.HandshakeIK, Initiator: false, Prologue: prologue,
		StaticKeypair: hostS, Random: bytes.NewReader(hostE.Private)}
	proto := "Noise_IK_25519_ChaChaPoly_SHA256"
	if psk != nil {
		ci.PresharedKey, ci.PresharedKeyPlacement = psk, 1
		cr.PresharedKey, cr.PresharedKeyPlacement = psk, 1
		proto = "Noise_IKpsk1_25519_ChaChaPoly_SHA256"
	}
	ini, err := noise.NewHandshakeState(ci)
	must(err)
	res, err := noise.NewHandshakeState(cr)
	must(err)
	m1, _, _, err := ini.WriteMessage(nil, []byte(p1))
	must(err)
	got1, _, _, err := res.ReadMessage(nil, m1)
	must(err)
	if string(got1) != p1 {
		panic("msg1 payload mismatch")
	}
	m2, hRecv, hSend, err := res.WriteMessage(nil, []byte(p2))
	must(err)
	got2, pSend, pRecv, err := ini.ReadMessage(nil, m2)
	must(err)
	if string(got2) != p2 {
		panic("msg2 payload mismatch")
	}
	if hx(ini.ChannelBinding()) != hx(res.ChannelBinding()) {
		panic("handshake hash mismatch")
	}
	t := transcript{Protocol: proto, PrologueHex: hx(prologue), PhoneEphPriv: hx(phoneE.Private), HostEphPriv: hx(hostE.Private),
		Msg1Payload: p1, Msg1Hex: hx(m1), Msg2Payload: p2, Msg2Hex: hx(m2), HandshakeHash: hx(ini.ChannelBinding())}
	if psk != nil {
		t.PskHex = hx(psk)
		t.PushKeyHex = hx(hk(ini.ChannelBinding(), "push-key", 32))
	}
	for _, f := range frames {
		var wire []byte
		if f.fromPhone {
			wire, err = pSend.Encrypt(nil, nil, f.pt)
			must(err)
			back, err := hRecv.Decrypt(nil, nil, wire)
			must(err)
			if hx(back) != hx(f.pt) {
				panic("transport mismatch")
			}
		} else {
			wire, err = hSend.Encrypt(nil, nil, f.pt)
			must(err)
			back, err := pRecv.Decrypt(nil, nil, wire)
			must(err)
			if hx(back) != hx(f.pt) {
				panic("transport mismatch")
			}
		}
		dir := "host->phone"
		if f.fromPhone {
			dir = "phone->host"
		}
		t.Transport = append(t.Transport, msg{Dir: dir, Plaintext: hx(f.pt), Note: f.note, Wire: hx(wire)})
	}
	return t
}

func main() {
	hostSign := ed25519.NewKeyFromSeed(seed("host-sign"))
	hostSignPub := hostSign.Public().(ed25519.PublicKey)
	phoneAuth := ed25519.NewKeyFromSeed(seed("phone-auth"))
	phoneAuthPub := phoneAuth.Public().(ed25519.PublicKey)
	hostS := x25519(seed("host-static"))
	phoneS := x25519(seed("phone-static"))

	idHash := sha256.Sum256(ctx("host-id", hostSignPub))
	hostID := b64(idHash[:16])
	pid := b64(seed("pid")[:16])
	nonce := seed("relay-challenge")

	hostAuthMsg := ctx("host-auth", []byte(hostID), nonce)
	phoneAuthMsg := ctx("phone-auth", []byte(hostID), nonce)

	claims := fmt.Sprintf(`{"hid":"%s","pid":"%s","pk":"%s","iat":1793000000,"exp":1800776000}`, hostID, pid, b64(phoneAuthPub))
	signed := "hpc1." + b64([]byte(claims))
	cred := signed + "." + b64(ed25519.Sign(hostSign, []byte(signed)))

	ps := seed("pairing-secret")
	eid := hk(ps, "enroll-id", 16)
	tag := hk(ps, "relay-tag", 32)
	tagHash := sha256.Sum256(tag)
	psk := hk(ps, "noise-psk", 32)

	prologue := ctx("noise", []byte(hostID))

	type fr = struct {
		fromPhone bool
		pt        []byte
		note      string
	}
	ping := []byte{0, 1, 2, 3, 4, 5, 6, 7}
	enroll := run(prologue, psk, phoneS, hostS, x25519(seed("phone-eph-enroll")), x25519(seed("host-eph-enroll")),
		fmt.Sprintf(`{"v":1,"name":"Test iPhone","pk":"%s"}`, b64(phoneAuthPub)),
		fmt.Sprintf(`{"v":1,"pid":"%s","cred":"%s"}`, pid, cred),
		[]fr{{true, frame(0x07, 0, ping), "PING stream 0 (confirms enrollment)"}, {false, frame(0x08, 0, ping), "PONG stream 0"}})

	reqHead := `{"method":"GET","path":"/v1/health","headers":[["accept","application/json"]]}`
	respHead := `{"status":200,"headers":[["content-type","application/json"]]}`
	body := `{"ok":true,"version":"1.15.0"}`
	session := run(prologue, nil, phoneS, hostS, x25519(seed("phone-eph-session")), x25519(seed("host-eph-session")),
		`{"v":1}`, `{"v":1}`,
		[]fr{
			{true, frame(0x01, 1, []byte(reqHead)), "OPEN stream 1 " + reqHead},
			{true, frame(0x03, 1, nil), "FIN stream 1"},
			{false, frame(0x05, 1, []byte(respHead)), "RESPONSE stream 1 " + respHead},
			{false, frame(0x02, 1, []byte(body)), "DATA stream 1 " + body},
			{false, frame(0x03, 1, nil), "FIN stream 1"},
		})

	pushKey, _ := hex.DecodeString(enroll.PushKeyHex)
	pushNonce := seed("push-nonce")[:12]
	pushPT := `{"v":1,"seq":1,"ts":1793000100,"kind":"message.done","chatId":"5b0e8c1e-2f39-4d55-9a57-0b7f6f3f1c11","title":"coder","body":"Done: all tests pass"}`
	aead, err := chacha20poly1305.New(pushKey)
	must(err)
	pushAD := ctx("push", []byte(hostID), []byte(pid))
	pushCT := append(append([]byte{}, pushNonce...), aead.Seal(nil, pushNonce, []byte(pushPT), pushAD)...)

	// Negative vectors: each MUST be rejected.
	badClaims := strings.Replace(claims, `"exp":1800776000`, `"exp":1900776000`, 1)
	credTampered := "hpc1." + b64([]byte(badClaims)) + cred[strings.LastIndex(cred, "."):]
	msg2, _ := hex.DecodeString(session.Msg2Hex)
	msg2[len(msg2)-1] ^= 0x01
	otherPrologue := ctx("noise", []byte("AAAAAAAAAAAAAAAAAAAAAA"))
	wrong := run(otherPrologue, nil, phoneS, hostS, x25519(seed("phone-eph-session")), x25519(seed("host-eph-session")), `{"v":1}`, `{"v":1}`, nil)

	out := map[string]any{
		"version": "hive-tunnel/1",
		"keys": map[string]string{
			"hostSignSeedHex":    hx(seed("host-sign")),
			"hostSignPub":        b64(hostSignPub),
			"hostStaticPrivHex":  hx(hostS.Private),
			"hostStaticPub":      b64(hostS.Public),
			"phoneAuthSeedHex":   hx(seed("phone-auth")),
			"phoneAuthPub":       b64(phoneAuthPub),
			"phoneStaticPrivHex": hx(phoneS.Private),
			"phoneStaticPub":     b64(phoneS.Public),
		},
		"hostId": hostID,
		"pid":    pid,
		"challenge": map[string]string{
			"nonce":             b64(nonce),
			"hostAuthInputHex":  hx(hostAuthMsg),
			"hostAuthSig":       b64(ed25519.Sign(hostSign, hostAuthMsg)),
			"phoneAuthInputHex": hx(phoneAuthMsg),
			"phoneAuthSig":      b64(ed25519.Sign(phoneAuth, phoneAuthMsg)),
		},
		"credential": map[string]string{"claimsJson": claims, "token": cred},
		"pairing": map[string]any{
			"pairingSecret": b64(ps),
			"eid":           b64(eid),
			"tag":           b64(tag),
			"tagHash":       b64(tagHash[:]),
			"pskHex":        hx(psk),
			"link": fmt.Sprintf("hive://pair?url=%s&token=%s&v=2&hid=%s&hs=%s&relay=%s&ps=%s&exp=1793000600",
				b64([]byte("http://100.101.102.104:8787")), "4f1c0de5a1b2c3d4e5f60718293a4b5c6d7e8f9011223344556677889900aabb",
				hostID, b64(hostS.Public), b64([]byte("https://relay.example.com")), b64(ps)),
		},
		"enrollment": enroll,
		"session":    session,
		"negative": map[string]string{
			"credentialTamperedClaims":        credTampered,
			"sessionMsg2TamperedHex":          hex.EncodeToString(msg2),
			"sessionMsg1OtherHostPrologueHex": wrong.Msg1Hex,
		},
		"push": map[string]string{
			"plaintext": pushPT,
			"adHex":     hx(pushAD),
			"nonceHex":  hx(pushNonce),
			"ct":        b64(pushCT),
		},
	}
	enc := json.NewEncoder(os.Stdout)
	enc.SetIndent("", "  ")
	enc.SetEscapeHTML(false)
	must(enc.Encode(out))
}

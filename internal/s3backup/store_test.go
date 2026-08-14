package s3backup

import (
	"bytes"
	"context"
	"errors"
	"fmt"
	"io"
	"net/http"
	"net/http/httptest"
	"os"
	"path/filepath"
	"testing"
	"time"
)

type fakeBackend struct {
	objects   []object
	data      map[string][]byte
	listErr   error
	getErr    error
	putErr    error
	listCalls int
	putKey    string
	putData   []byte
}

func (f *fakeBackend) list(context.Context, string, string) ([]object, error) {
	f.listCalls++
	return f.objects, f.listErr
}

func (f *fakeBackend) put(_ context.Context, _, key string, body io.ReadSeeker, _ int64) error {
	f.putKey = key
	f.putData, _ = io.ReadAll(body)
	return f.putErr
}

func (f *fakeBackend) get(_ context.Context, _, key string, destination io.Writer) error {
	if f.getErr != nil {
		return f.getErr
	}
	_, err := io.Copy(destination, bytes.NewReader(f.data[key]))
	return err
}

func TestRestoreIfMissingUsesNewestBackup(t *testing.T) {
	now := time.Now()
	backend := &fakeBackend{
		objects: []object{
			{key: "prod/mneme-old.db", modified: now.Add(-time.Hour)},
			{key: "prod/ignore.txt", modified: now.Add(time.Hour)},
			{key: "prod/mneme-new.db", modified: now},
		},
		data: map[string][]byte{"prod/mneme-new.db": []byte("sqlite database")},
	}
	store := newStore(Config{Bucket: "bucket", Prefix: "prod"}, backend)
	databasePath := filepath.Join(t.TempDir(), "nested", "companion.db")
	restored, key, err := store.RestoreIfMissing(context.Background(), databasePath)
	if err != nil {
		t.Fatal(err)
	}
	if !restored || key != "prod/mneme-new.db" {
		t.Fatalf("unexpected restore result: %v %q", restored, key)
	}
	content, err := os.ReadFile(databasePath)
	if err != nil || string(content) != "sqlite database" {
		t.Fatalf("unexpected restored content: %q %v", content, err)
	}
	if info, err := os.Stat(databasePath); err != nil || info.Mode().Perm() != 0o600 {
		t.Fatalf("unexpected restored permissions: %v %v", info, err)
	}
}

func TestRestoreDoesNotInspectS3WhenLocalDatabaseExists(t *testing.T) {
	backend := &fakeBackend{listErr: errors.New("should not be called")}
	store := newStore(Config{Bucket: "bucket"}, backend)
	databasePath := filepath.Join(t.TempDir(), "companion.db")
	if err := os.WriteFile(databasePath, []byte("local"), 0o600); err != nil {
		t.Fatal(err)
	}
	restored, _, err := store.RestoreIfMissing(context.Background(), databasePath)
	if err != nil || restored || backend.listCalls != 0 {
		t.Fatalf("local database was not preferred: restored=%v calls=%d err=%v", restored, backend.listCalls, err)
	}
}

func TestRestoreFailureLeavesDatabaseMissing(t *testing.T) {
	backend := &fakeBackend{
		objects: []object{{key: "mneme.db", modified: time.Now()}},
		getErr:  errors.New("download interrupted"),
	}
	store := newStore(Config{Bucket: "bucket"}, backend)
	databasePath := filepath.Join(t.TempDir(), "companion.db")
	if _, _, err := store.RestoreIfMissing(context.Background(), databasePath); err == nil {
		t.Fatal("expected restore error")
	}
	if _, err := os.Stat(databasePath); !os.IsNotExist(err) {
		t.Fatalf("partial database was installed: %v", err)
	}
}

func TestUploadUsesPrefixAndRejectsEmptyFile(t *testing.T) {
	backend := &fakeBackend{}
	store := newStore(Config{Bucket: "bucket", Prefix: "/prod/backups/"}, backend)
	directory := t.TempDir()
	backupPath := filepath.Join(directory, "mneme-20260814.db")
	if err := os.WriteFile(backupPath, []byte("snapshot"), 0o600); err != nil {
		t.Fatal(err)
	}
	key, err := store.Upload(context.Background(), backupPath)
	if err != nil || key != "prod/backups/mneme-20260814.db" || string(backend.putData) != "snapshot" {
		t.Fatalf("unexpected upload: key=%q data=%q err=%v", key, backend.putData, err)
	}
	empty := filepath.Join(directory, "empty.db")
	if err := os.WriteFile(empty, nil, 0o600); err != nil {
		t.Fatal(err)
	}
	if _, err := store.Upload(context.Background(), empty); err == nil {
		t.Fatal("expected empty backup error")
	}
}

func TestAWSBackendWithPathStyleEndpoint(t *testing.T) {
	t.Setenv("AWS_ACCESS_KEY_ID", "test-access-key")
	t.Setenv("AWS_SECRET_ACCESS_KEY", "test-secret-key")
	t.Setenv("AWS_EC2_METADATA_DISABLED", "true")
	var uploaded []byte
	server := httptest.NewServer(http.HandlerFunc(func(response http.ResponseWriter, request *http.Request) {
		switch {
		case request.Method == http.MethodGet && request.URL.Query().Get("list-type") == "2":
			response.Header().Set("Content-Type", "application/xml")
			_, _ = fmt.Fprint(response, `<?xml version="1.0" encoding="UTF-8"?>
<ListBucketResult xmlns="http://s3.amazonaws.com/doc/2006-03-01/"><Name>bucket</Name><Prefix>prod/backups/</Prefix><KeyCount>1</KeyCount><MaxKeys>1000</MaxKeys><IsTruncated>false</IsTruncated><Contents><Key>prod/backups/mneme-latest.db</Key><LastModified>2026-08-14T12:00:00.000Z</LastModified><ETag>&quot;etag&quot;</ETag><Size>13</Size><StorageClass>STANDARD</StorageClass></Contents></ListBucketResult>`)
		case request.Method == http.MethodGet && request.URL.Path == "/bucket/prod/backups/mneme-latest.db":
			_, _ = response.Write([]byte("remote sqlite"))
		case request.Method == http.MethodPut && request.URL.Path == "/bucket/prod/backups/companion.db":
			uploaded, _ = io.ReadAll(request.Body)
			response.Header().Set("ETag", `"etag"`)
		default:
			http.Error(response, "unexpected request", http.StatusBadRequest)
			t.Errorf("unexpected S3 request: %s %s", request.Method, request.URL.String())
		}
	}))
	defer server.Close()

	store, err := New(context.Background(), Config{
		Bucket: "bucket", Prefix: "prod/backups", Region: "us-east-1", Endpoint: server.URL,
	})
	if err != nil {
		t.Fatal(err)
	}
	databasePath := filepath.Join(t.TempDir(), "companion.db")
	if restored, _, err := store.RestoreIfMissing(context.Background(), databasePath); err != nil || !restored {
		t.Fatalf("restore through AWS backend failed: restored=%v err=%v", restored, err)
	}
	if key, err := store.Upload(context.Background(), databasePath); err != nil || key != "prod/backups/companion.db" {
		t.Fatalf("upload through AWS backend failed: key=%q err=%v", key, err)
	}
	if !bytes.Contains(uploaded, []byte("remote sqlite")) {
		t.Fatalf("unexpected uploaded data: %q", uploaded)
	}
}

/*
 * onnxruntime <-> GCC 12 libstdc++ 호환 심(shim) — Linux 컨테이너 빌드 전용.
 *
 * [문제]
 * 로컬 임베딩(fastembed → ort)은 `ort download-binaries`로 pyke가 배포하는 프리빌트
 * onnxruntime(aarch64) 정적 바이너리를 빌드시 내려받아 정적 링크한다. 이 바이너리는
 * GCC <= 11 툴체인으로 빌드돼 libstdc++ 내부 심볼 `__cxa_call_terminate`를 참조한다.
 * 그런데 이 심볼은 GCC 12에서 제거됐고, 컨테이너 베이스(debian trixie, libstdc++
 * GCC14)에는 존재하지 않는다 → 최종 링크에서 `undefined reference` 로 빌드가 깨진다.
 *
 * (프리빌트 onnxruntime은 반대로 최신 glibc 심볼도 요구한다 — `__libc_single_threaded`
 *  (glibc 2.32+), `__isoc23_strtol` 계열(glibc 2.38+). 이 glibc 요구는 베이스를
 *  trixie(glibc 2.41)로 올려 충족한다. bookworm(glibc 2.36)은 isoc23 계열이 없어 깨진다.
 *  즉 glibc 요구는 배포판 선택으로, libstdc++ 심볼 하나만 남아 여기서 그것을 채운다.
 *  상세 근거는 Dockerfile Stage 2 주석 참조.)
 *
 * [해결]
 * `__cxa_call_terminate`는 "예외가 noexcept 경계를 탈출"하는 상황에서만 호출되는
 * 종결(termination) 경로다. 이때의 표준 동작은 std::terminate() 호출이고, 기본
 * terminate 핸들러는 abort()다 (Maia도 onnxruntime도 커스텀 terminate 핸들러를
 * 설치하지 않는다). 따라서 예외 인자를 무시하고 abort()로 위임하는 최소 구현이
 * 동작상 동치다. 프로세스는 어차피 종료되는 경로이므로 정보 유실·상태 오염 위험이 없다.
 *
 * 이 파일은 오직 컨테이너 빌드에서만(RUSTFLAGS로) 링크된다. macOS 로컬 빌드는
 * libc++를 쓰므로 이 심볼을 요구하지 않으며 이 파일과 무관하다.
 *
 * 출처: 프리빌트 onnxruntime 링크 에러(ort-sys rlib, subgraph_base.cc.o). 검증일 2026-07-07.
 */

/* abort()는 libc 심볼이라 libstdc++에 의존하지 않는다(심을 자기완결적으로 유지). */
extern void abort(void);

void __cxa_call_terminate(void *exception_ptr) {
    (void)exception_ptr;
    abort();
}

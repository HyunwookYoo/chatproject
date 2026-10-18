// GENERATED CODE - DO NOT MODIFY BY HAND
// coverage:ignore-file
// ignore_for_file: type=lint
// ignore_for_file: unused_element, deprecated_member_use, deprecated_member_use_from_same_package, use_function_type_syntax_for_parameters, unnecessary_const, avoid_init_to_null, invalid_override_different_default_values_named, prefer_expression_function_bodies, annotate_overrides, invalid_annotation_target, unnecessary_question_mark

part of 'chat.dart';

// **************************************************************************
// FreezedGenerator
// **************************************************************************

T _$identity<T>(T value) => value;

/// @nodoc
mixin _$ChatEvent {
  @override
  bool operator ==(Object other) {
    return identical(this, other) ||
        (other.runtimeType == runtimeType && other is ChatEvent);
  }

  @override
  int get hashCode => runtimeType.hashCode;

  @override
  String toString() {
    return 'ChatEvent()';
  }
}

/// @nodoc
class $ChatEventCopyWith<$Res> {
  $ChatEventCopyWith(ChatEvent _, $Res Function(ChatEvent) __);
}

/// Adds pattern-matching-related methods to [ChatEvent].
extension ChatEventPatterns on ChatEvent {
  /// A variant of `map` that fallback to returning `orElse`.
  ///
  /// It is equivalent to doing:
  /// ```dart
  /// switch (sealedClass) {
  ///   case final Subclass value:
  ///     return ...;
  ///   case _:
  ///     return orElse();
  /// }
  /// ```

  @optionalTypeArgs
  TResult maybeMap<TResult extends Object?>({
    TResult Function(ChatEvent_AccountChanged value)? accountChanged,
    TResult Function(ChatEvent_AccountAuthChanged value)? accountAuthChanged,
    TResult Function(ChatEvent_AccountCacheUsed value)? accountCacheUsed,
    TResult Function(ChatEvent_ConnectionState value)? connectionState,
    TResult Function(ChatEvent_Error value)? error,
    TResult Function(ChatEvent_NetworkChanged value)? networkChanged,
    TResult Function(ChatEvent_PeerChanged value)? peerChanged,
    TResult Function(ChatEvent_ProbeTextReceived value)? probeTextReceived,
    required TResult orElse(),
  }) {
    final _that = this;
    switch (_that) {
      case ChatEvent_AccountChanged() when accountChanged != null:
        return accountChanged(_that);
      case ChatEvent_AccountAuthChanged() when accountAuthChanged != null:
        return accountAuthChanged(_that);
      case ChatEvent_AccountCacheUsed() when accountCacheUsed != null:
        return accountCacheUsed(_that);
      case ChatEvent_ConnectionState() when connectionState != null:
        return connectionState(_that);
      case ChatEvent_Error() when error != null:
        return error(_that);
      case ChatEvent_NetworkChanged() when networkChanged != null:
        return networkChanged(_that);
      case ChatEvent_PeerChanged() when peerChanged != null:
        return peerChanged(_that);
      case ChatEvent_ProbeTextReceived() when probeTextReceived != null:
        return probeTextReceived(_that);
      case _:
        return orElse();
    }
  }

  /// A `switch`-like method, using callbacks.
  ///
  /// Callbacks receives the raw object, upcasted.
  /// It is equivalent to doing:
  /// ```dart
  /// switch (sealedClass) {
  ///   case final Subclass value:
  ///     return ...;
  ///   case final Subclass2 value:
  ///     return ...;
  /// }
  /// ```

  @optionalTypeArgs
  TResult map<TResult extends Object?>({
    required TResult Function(ChatEvent_AccountChanged value) accountChanged,
    required TResult Function(ChatEvent_AccountAuthChanged value)
    accountAuthChanged,
    required TResult Function(ChatEvent_AccountCacheUsed value)
    accountCacheUsed,
    required TResult Function(ChatEvent_ConnectionState value) connectionState,
    required TResult Function(ChatEvent_Error value) error,
    required TResult Function(ChatEvent_NetworkChanged value) networkChanged,
    required TResult Function(ChatEvent_PeerChanged value) peerChanged,
    required TResult Function(ChatEvent_ProbeTextReceived value)
    probeTextReceived,
  }) {
    final _that = this;
    switch (_that) {
      case ChatEvent_AccountChanged():
        return accountChanged(_that);
      case ChatEvent_AccountAuthChanged():
        return accountAuthChanged(_that);
      case ChatEvent_AccountCacheUsed():
        return accountCacheUsed(_that);
      case ChatEvent_ConnectionState():
        return connectionState(_that);
      case ChatEvent_Error():
        return error(_that);
      case ChatEvent_NetworkChanged():
        return networkChanged(_that);
      case ChatEvent_PeerChanged():
        return peerChanged(_that);
      case ChatEvent_ProbeTextReceived():
        return probeTextReceived(_that);
    }
  }

  /// A variant of `map` that fallback to returning `null`.
  ///
  /// It is equivalent to doing:
  /// ```dart
  /// switch (sealedClass) {
  ///   case final Subclass value:
  ///     return ...;
  ///   case _:
  ///     return null;
  /// }
  /// ```

  @optionalTypeArgs
  TResult? mapOrNull<TResult extends Object?>({
    TResult? Function(ChatEvent_AccountChanged value)? accountChanged,
    TResult? Function(ChatEvent_AccountAuthChanged value)? accountAuthChanged,
    TResult? Function(ChatEvent_AccountCacheUsed value)? accountCacheUsed,
    TResult? Function(ChatEvent_ConnectionState value)? connectionState,
    TResult? Function(ChatEvent_Error value)? error,
    TResult? Function(ChatEvent_NetworkChanged value)? networkChanged,
    TResult? Function(ChatEvent_PeerChanged value)? peerChanged,
    TResult? Function(ChatEvent_ProbeTextReceived value)? probeTextReceived,
  }) {
    final _that = this;
    switch (_that) {
      case ChatEvent_AccountChanged() when accountChanged != null:
        return accountChanged(_that);
      case ChatEvent_AccountAuthChanged() when accountAuthChanged != null:
        return accountAuthChanged(_that);
      case ChatEvent_AccountCacheUsed() when accountCacheUsed != null:
        return accountCacheUsed(_that);
      case ChatEvent_ConnectionState() when connectionState != null:
        return connectionState(_that);
      case ChatEvent_Error() when error != null:
        return error(_that);
      case ChatEvent_NetworkChanged() when networkChanged != null:
        return networkChanged(_that);
      case ChatEvent_PeerChanged() when peerChanged != null:
        return peerChanged(_that);
      case ChatEvent_ProbeTextReceived() when probeTextReceived != null:
        return probeTextReceived(_that);
      case _:
        return null;
    }
  }

  /// A variant of `when` that fallback to an `orElse` callback.
  ///
  /// It is equivalent to doing:
  /// ```dart
  /// switch (sealedClass) {
  ///   case Subclass(:final field):
  ///     return ...;
  ///   case _:
  ///     return orElse();
  /// }
  /// ```

  @optionalTypeArgs
  TResult maybeWhen<TResult extends Object?>({
    TResult Function(BigInt revision, AccountView view)? accountChanged,
    TResult Function(String peerId, String installationId, bool ready)?
    accountAuthChanged,
    TResult Function(String peerId)? accountCacheUsed,
    TResult Function(int directPeers, bool mailboxOk, bool? queueOk)?
    connectionState,
    TResult Function(ErrorCode code, String detail)? error,
    TResult Function(NetworkInfo info)? networkChanged,
    TResult Function(BigInt revision, PeerInfo peer)? peerChanged,
    TResult Function(String peerId, String text)? probeTextReceived,
    required TResult orElse(),
  }) {
    final _that = this;
    switch (_that) {
      case ChatEvent_AccountChanged() when accountChanged != null:
        return accountChanged(_that.revision, _that.view);
      case ChatEvent_AccountAuthChanged() when accountAuthChanged != null:
        return accountAuthChanged(
          _that.peerId,
          _that.installationId,
          _that.ready,
        );
      case ChatEvent_AccountCacheUsed() when accountCacheUsed != null:
        return accountCacheUsed(_that.peerId);
      case ChatEvent_ConnectionState() when connectionState != null:
        return connectionState(
          _that.directPeers,
          _that.mailboxOk,
          _that.queueOk,
        );
      case ChatEvent_Error() when error != null:
        return error(_that.code, _that.detail);
      case ChatEvent_NetworkChanged() when networkChanged != null:
        return networkChanged(_that.info);
      case ChatEvent_PeerChanged() when peerChanged != null:
        return peerChanged(_that.revision, _that.peer);
      case ChatEvent_ProbeTextReceived() when probeTextReceived != null:
        return probeTextReceived(_that.peerId, _that.text);
      case _:
        return orElse();
    }
  }

  /// A `switch`-like method, using callbacks.
  ///
  /// As opposed to `map`, this offers destructuring.
  /// It is equivalent to doing:
  /// ```dart
  /// switch (sealedClass) {
  ///   case Subclass(:final field):
  ///     return ...;
  ///   case Subclass2(:final field2):
  ///     return ...;
  /// }
  /// ```

  @optionalTypeArgs
  TResult when<TResult extends Object?>({
    required TResult Function(BigInt revision, AccountView view) accountChanged,
    required TResult Function(String peerId, String installationId, bool ready)
    accountAuthChanged,
    required TResult Function(String peerId) accountCacheUsed,
    required TResult Function(int directPeers, bool mailboxOk, bool? queueOk)
    connectionState,
    required TResult Function(ErrorCode code, String detail) error,
    required TResult Function(NetworkInfo info) networkChanged,
    required TResult Function(BigInt revision, PeerInfo peer) peerChanged,
    required TResult Function(String peerId, String text) probeTextReceived,
  }) {
    final _that = this;
    switch (_that) {
      case ChatEvent_AccountChanged():
        return accountChanged(_that.revision, _that.view);
      case ChatEvent_AccountAuthChanged():
        return accountAuthChanged(
          _that.peerId,
          _that.installationId,
          _that.ready,
        );
      case ChatEvent_AccountCacheUsed():
        return accountCacheUsed(_that.peerId);
      case ChatEvent_ConnectionState():
        return connectionState(
          _that.directPeers,
          _that.mailboxOk,
          _that.queueOk,
        );
      case ChatEvent_Error():
        return error(_that.code, _that.detail);
      case ChatEvent_NetworkChanged():
        return networkChanged(_that.info);
      case ChatEvent_PeerChanged():
        return peerChanged(_that.revision, _that.peer);
      case ChatEvent_ProbeTextReceived():
        return probeTextReceived(_that.peerId, _that.text);
    }
  }

  /// A variant of `when` that fallback to returning `null`
  ///
  /// It is equivalent to doing:
  /// ```dart
  /// switch (sealedClass) {
  ///   case Subclass(:final field):
  ///     return ...;
  ///   case _:
  ///     return null;
  /// }
  /// ```

  @optionalTypeArgs
  TResult? whenOrNull<TResult extends Object?>({
    TResult? Function(BigInt revision, AccountView view)? accountChanged,
    TResult? Function(String peerId, String installationId, bool ready)?
    accountAuthChanged,
    TResult? Function(String peerId)? accountCacheUsed,
    TResult? Function(int directPeers, bool mailboxOk, bool? queueOk)?
    connectionState,
    TResult? Function(ErrorCode code, String detail)? error,
    TResult? Function(NetworkInfo info)? networkChanged,
    TResult? Function(BigInt revision, PeerInfo peer)? peerChanged,
    TResult? Function(String peerId, String text)? probeTextReceived,
  }) {
    final _that = this;
    switch (_that) {
      case ChatEvent_AccountChanged() when accountChanged != null:
        return accountChanged(_that.revision, _that.view);
      case ChatEvent_AccountAuthChanged() when accountAuthChanged != null:
        return accountAuthChanged(
          _that.peerId,
          _that.installationId,
          _that.ready,
        );
      case ChatEvent_AccountCacheUsed() when accountCacheUsed != null:
        return accountCacheUsed(_that.peerId);
      case ChatEvent_ConnectionState() when connectionState != null:
        return connectionState(
          _that.directPeers,
          _that.mailboxOk,
          _that.queueOk,
        );
      case ChatEvent_Error() when error != null:
        return error(_that.code, _that.detail);
      case ChatEvent_NetworkChanged() when networkChanged != null:
        return networkChanged(_that.info);
      case ChatEvent_PeerChanged() when peerChanged != null:
        return peerChanged(_that.revision, _that.peer);
      case ChatEvent_ProbeTextReceived() when probeTextReceived != null:
        return probeTextReceived(_that.peerId, _that.text);
      case _:
        return null;
    }
  }
}

/// @nodoc

class ChatEvent_AccountChanged extends ChatEvent {
  const ChatEvent_AccountChanged({required this.revision, required this.view})
    : super._();

  final BigInt revision;
  final AccountView view;

  /// Create a copy of ChatEvent
  /// with the given fields replaced by the non-null parameter values.
  @JsonKey(includeFromJson: false, includeToJson: false)
  @pragma('vm:prefer-inline')
  $ChatEvent_AccountChangedCopyWith<ChatEvent_AccountChanged> get copyWith =>
      _$ChatEvent_AccountChangedCopyWithImpl<ChatEvent_AccountChanged>(
        this,
        _$identity,
      );

  @override
  bool operator ==(Object other) {
    return identical(this, other) ||
        (other.runtimeType == runtimeType &&
            other is ChatEvent_AccountChanged &&
            (identical(other.revision, revision) ||
                other.revision == revision) &&
            (identical(other.view, view) || other.view == view));
  }

  @override
  int get hashCode => Object.hash(runtimeType, revision, view);

  @override
  String toString() {
    return 'ChatEvent.accountChanged(revision: $revision, view: $view)';
  }
}

/// @nodoc
abstract mixin class $ChatEvent_AccountChangedCopyWith<$Res>
    implements $ChatEventCopyWith<$Res> {
  factory $ChatEvent_AccountChangedCopyWith(
    ChatEvent_AccountChanged value,
    $Res Function(ChatEvent_AccountChanged) _then,
  ) = _$ChatEvent_AccountChangedCopyWithImpl;
  @useResult
  $Res call({BigInt revision, AccountView view});
}

/// @nodoc
class _$ChatEvent_AccountChangedCopyWithImpl<$Res>
    implements $ChatEvent_AccountChangedCopyWith<$Res> {
  _$ChatEvent_AccountChangedCopyWithImpl(this._self, this._then);

  final ChatEvent_AccountChanged _self;
  final $Res Function(ChatEvent_AccountChanged) _then;

  /// Create a copy of ChatEvent
  /// with the given fields replaced by the non-null parameter values.
  @pragma('vm:prefer-inline')
  $Res call({Object? revision = null, Object? view = null}) {
    return _then(
      ChatEvent_AccountChanged(
        revision: null == revision
            ? _self.revision
            : revision // ignore: cast_nullable_to_non_nullable
                  as BigInt,
        view: null == view
            ? _self.view
            : view // ignore: cast_nullable_to_non_nullable
                  as AccountView,
      ),
    );
  }
}

/// @nodoc

class ChatEvent_AccountAuthChanged extends ChatEvent {
  const ChatEvent_AccountAuthChanged({
    required this.peerId,
    required this.installationId,
    required this.ready,
  }) : super._();

  final String peerId;
  final String installationId;
  final bool ready;

  /// Create a copy of ChatEvent
  /// with the given fields replaced by the non-null parameter values.
  @JsonKey(includeFromJson: false, includeToJson: false)
  @pragma('vm:prefer-inline')
  $ChatEvent_AccountAuthChangedCopyWith<ChatEvent_AccountAuthChanged>
  get copyWith =>
      _$ChatEvent_AccountAuthChangedCopyWithImpl<ChatEvent_AccountAuthChanged>(
        this,
        _$identity,
      );

  @override
  bool operator ==(Object other) {
    return identical(this, other) ||
        (other.runtimeType == runtimeType &&
            other is ChatEvent_AccountAuthChanged &&
            (identical(other.peerId, peerId) || other.peerId == peerId) &&
            (identical(other.installationId, installationId) ||
                other.installationId == installationId) &&
            (identical(other.ready, ready) || other.ready == ready));
  }

  @override
  int get hashCode => Object.hash(runtimeType, peerId, installationId, ready);

  @override
  String toString() {
    return 'ChatEvent.accountAuthChanged(peerId: $peerId, installationId: $installationId, ready: $ready)';
  }
}

/// @nodoc
abstract mixin class $ChatEvent_AccountAuthChangedCopyWith<$Res>
    implements $ChatEventCopyWith<$Res> {
  factory $ChatEvent_AccountAuthChangedCopyWith(
    ChatEvent_AccountAuthChanged value,
    $Res Function(ChatEvent_AccountAuthChanged) _then,
  ) = _$ChatEvent_AccountAuthChangedCopyWithImpl;
  @useResult
  $Res call({String peerId, String installationId, bool ready});
}

/// @nodoc
class _$ChatEvent_AccountAuthChangedCopyWithImpl<$Res>
    implements $ChatEvent_AccountAuthChangedCopyWith<$Res> {
  _$ChatEvent_AccountAuthChangedCopyWithImpl(this._self, this._then);

  final ChatEvent_AccountAuthChanged _self;
  final $Res Function(ChatEvent_AccountAuthChanged) _then;

  /// Create a copy of ChatEvent
  /// with the given fields replaced by the non-null parameter values.
  @pragma('vm:prefer-inline')
  $Res call({
    Object? peerId = null,
    Object? installationId = null,
    Object? ready = null,
  }) {
    return _then(
      ChatEvent_AccountAuthChanged(
        peerId: null == peerId
            ? _self.peerId
            : peerId // ignore: cast_nullable_to_non_nullable
                  as String,
        installationId: null == installationId
            ? _self.installationId
            : installationId // ignore: cast_nullable_to_non_nullable
                  as String,
        ready: null == ready
            ? _self.ready
            : ready // ignore: cast_nullable_to_non_nullable
                  as bool,
      ),
    );
  }
}

/// @nodoc

class ChatEvent_AccountCacheUsed extends ChatEvent {
  const ChatEvent_AccountCacheUsed({required this.peerId}) : super._();

  final String peerId;

  /// Create a copy of ChatEvent
  /// with the given fields replaced by the non-null parameter values.
  @JsonKey(includeFromJson: false, includeToJson: false)
  @pragma('vm:prefer-inline')
  $ChatEvent_AccountCacheUsedCopyWith<ChatEvent_AccountCacheUsed>
  get copyWith =>
      _$ChatEvent_AccountCacheUsedCopyWithImpl<ChatEvent_AccountCacheUsed>(
        this,
        _$identity,
      );

  @override
  bool operator ==(Object other) {
    return identical(this, other) ||
        (other.runtimeType == runtimeType &&
            other is ChatEvent_AccountCacheUsed &&
            (identical(other.peerId, peerId) || other.peerId == peerId));
  }

  @override
  int get hashCode => Object.hash(runtimeType, peerId);

  @override
  String toString() {
    return 'ChatEvent.accountCacheUsed(peerId: $peerId)';
  }
}

/// @nodoc
abstract mixin class $ChatEvent_AccountCacheUsedCopyWith<$Res>
    implements $ChatEventCopyWith<$Res> {
  factory $ChatEvent_AccountCacheUsedCopyWith(
    ChatEvent_AccountCacheUsed value,
    $Res Function(ChatEvent_AccountCacheUsed) _then,
  ) = _$ChatEvent_AccountCacheUsedCopyWithImpl;
  @useResult
  $Res call({String peerId});
}

/// @nodoc
class _$ChatEvent_AccountCacheUsedCopyWithImpl<$Res>
    implements $ChatEvent_AccountCacheUsedCopyWith<$Res> {
  _$ChatEvent_AccountCacheUsedCopyWithImpl(this._self, this._then);

  final ChatEvent_AccountCacheUsed _self;
  final $Res Function(ChatEvent_AccountCacheUsed) _then;

  /// Create a copy of ChatEvent
  /// with the given fields replaced by the non-null parameter values.
  @pragma('vm:prefer-inline')
  $Res call({Object? peerId = null}) {
    return _then(
      ChatEvent_AccountCacheUsed(
        peerId: null == peerId
            ? _self.peerId
            : peerId // ignore: cast_nullable_to_non_nullable
                  as String,
      ),
    );
  }
}

/// @nodoc

class ChatEvent_ConnectionState extends ChatEvent {
  const ChatEvent_ConnectionState({
    required this.directPeers,
    required this.mailboxOk,
    this.queueOk,
  }) : super._();

  final int directPeers;
  final bool mailboxOk;
  final bool? queueOk;

  /// Create a copy of ChatEvent
  /// with the given fields replaced by the non-null parameter values.
  @JsonKey(includeFromJson: false, includeToJson: false)
  @pragma('vm:prefer-inline')
  $ChatEvent_ConnectionStateCopyWith<ChatEvent_ConnectionState> get copyWith =>
      _$ChatEvent_ConnectionStateCopyWithImpl<ChatEvent_ConnectionState>(
        this,
        _$identity,
      );

  @override
  bool operator ==(Object other) {
    return identical(this, other) ||
        (other.runtimeType == runtimeType &&
            other is ChatEvent_ConnectionState &&
            (identical(other.directPeers, directPeers) ||
                other.directPeers == directPeers) &&
            (identical(other.mailboxOk, mailboxOk) ||
                other.mailboxOk == mailboxOk) &&
            (identical(other.queueOk, queueOk) || other.queueOk == queueOk));
  }

  @override
  int get hashCode => Object.hash(runtimeType, directPeers, mailboxOk, queueOk);

  @override
  String toString() {
    return 'ChatEvent.connectionState(directPeers: $directPeers, mailboxOk: $mailboxOk, queueOk: $queueOk)';
  }
}

/// @nodoc
abstract mixin class $ChatEvent_ConnectionStateCopyWith<$Res>
    implements $ChatEventCopyWith<$Res> {
  factory $ChatEvent_ConnectionStateCopyWith(
    ChatEvent_ConnectionState value,
    $Res Function(ChatEvent_ConnectionState) _then,
  ) = _$ChatEvent_ConnectionStateCopyWithImpl;
  @useResult
  $Res call({int directPeers, bool mailboxOk, bool? queueOk});
}

/// @nodoc
class _$ChatEvent_ConnectionStateCopyWithImpl<$Res>
    implements $ChatEvent_ConnectionStateCopyWith<$Res> {
  _$ChatEvent_ConnectionStateCopyWithImpl(this._self, this._then);

  final ChatEvent_ConnectionState _self;
  final $Res Function(ChatEvent_ConnectionState) _then;

  /// Create a copy of ChatEvent
  /// with the given fields replaced by the non-null parameter values.
  @pragma('vm:prefer-inline')
  $Res call({
    Object? directPeers = null,
    Object? mailboxOk = null,
    Object? queueOk = freezed,
  }) {
    return _then(
      ChatEvent_ConnectionState(
        directPeers: null == directPeers
            ? _self.directPeers
            : directPeers // ignore: cast_nullable_to_non_nullable
                  as int,
        mailboxOk: null == mailboxOk
            ? _self.mailboxOk
            : mailboxOk // ignore: cast_nullable_to_non_nullable
                  as bool,
        queueOk: freezed == queueOk
            ? _self.queueOk
            : queueOk // ignore: cast_nullable_to_non_nullable
                  as bool?,
      ),
    );
  }
}

/// @nodoc

class ChatEvent_Error extends ChatEvent {
  const ChatEvent_Error({required this.code, required this.detail}) : super._();

  final ErrorCode code;
  final String detail;

  /// Create a copy of ChatEvent
  /// with the given fields replaced by the non-null parameter values.
  @JsonKey(includeFromJson: false, includeToJson: false)
  @pragma('vm:prefer-inline')
  $ChatEvent_ErrorCopyWith<ChatEvent_Error> get copyWith =>
      _$ChatEvent_ErrorCopyWithImpl<ChatEvent_Error>(this, _$identity);

  @override
  bool operator ==(Object other) {
    return identical(this, other) ||
        (other.runtimeType == runtimeType &&
            other is ChatEvent_Error &&
            (identical(other.code, code) || other.code == code) &&
            (identical(other.detail, detail) || other.detail == detail));
  }

  @override
  int get hashCode => Object.hash(runtimeType, code, detail);

  @override
  String toString() {
    return 'ChatEvent.error(code: $code, detail: $detail)';
  }
}

/// @nodoc
abstract mixin class $ChatEvent_ErrorCopyWith<$Res>
    implements $ChatEventCopyWith<$Res> {
  factory $ChatEvent_ErrorCopyWith(
    ChatEvent_Error value,
    $Res Function(ChatEvent_Error) _then,
  ) = _$ChatEvent_ErrorCopyWithImpl;
  @useResult
  $Res call({ErrorCode code, String detail});
}

/// @nodoc
class _$ChatEvent_ErrorCopyWithImpl<$Res>
    implements $ChatEvent_ErrorCopyWith<$Res> {
  _$ChatEvent_ErrorCopyWithImpl(this._self, this._then);

  final ChatEvent_Error _self;
  final $Res Function(ChatEvent_Error) _then;

  /// Create a copy of ChatEvent
  /// with the given fields replaced by the non-null parameter values.
  @pragma('vm:prefer-inline')
  $Res call({Object? code = null, Object? detail = null}) {
    return _then(
      ChatEvent_Error(
        code: null == code
            ? _self.code
            : code // ignore: cast_nullable_to_non_nullable
                  as ErrorCode,
        detail: null == detail
            ? _self.detail
            : detail // ignore: cast_nullable_to_non_nullable
                  as String,
      ),
    );
  }
}

/// @nodoc

class ChatEvent_NetworkChanged extends ChatEvent {
  const ChatEvent_NetworkChanged({required this.info}) : super._();

  final NetworkInfo info;

  /// Create a copy of ChatEvent
  /// with the given fields replaced by the non-null parameter values.
  @JsonKey(includeFromJson: false, includeToJson: false)
  @pragma('vm:prefer-inline')
  $ChatEvent_NetworkChangedCopyWith<ChatEvent_NetworkChanged> get copyWith =>
      _$ChatEvent_NetworkChangedCopyWithImpl<ChatEvent_NetworkChanged>(
        this,
        _$identity,
      );

  @override
  bool operator ==(Object other) {
    return identical(this, other) ||
        (other.runtimeType == runtimeType &&
            other is ChatEvent_NetworkChanged &&
            (identical(other.info, info) || other.info == info));
  }

  @override
  int get hashCode => Object.hash(runtimeType, info);

  @override
  String toString() {
    return 'ChatEvent.networkChanged(info: $info)';
  }
}

/// @nodoc
abstract mixin class $ChatEvent_NetworkChangedCopyWith<$Res>
    implements $ChatEventCopyWith<$Res> {
  factory $ChatEvent_NetworkChangedCopyWith(
    ChatEvent_NetworkChanged value,
    $Res Function(ChatEvent_NetworkChanged) _then,
  ) = _$ChatEvent_NetworkChangedCopyWithImpl;
  @useResult
  $Res call({NetworkInfo info});
}

/// @nodoc
class _$ChatEvent_NetworkChangedCopyWithImpl<$Res>
    implements $ChatEvent_NetworkChangedCopyWith<$Res> {
  _$ChatEvent_NetworkChangedCopyWithImpl(this._self, this._then);

  final ChatEvent_NetworkChanged _self;
  final $Res Function(ChatEvent_NetworkChanged) _then;

  /// Create a copy of ChatEvent
  /// with the given fields replaced by the non-null parameter values.
  @pragma('vm:prefer-inline')
  $Res call({Object? info = null}) {
    return _then(
      ChatEvent_NetworkChanged(
        info: null == info
            ? _self.info
            : info // ignore: cast_nullable_to_non_nullable
                  as NetworkInfo,
      ),
    );
  }
}

/// @nodoc

class ChatEvent_PeerChanged extends ChatEvent {
  const ChatEvent_PeerChanged({required this.revision, required this.peer})
    : super._();

  final BigInt revision;
  final PeerInfo peer;

  /// Create a copy of ChatEvent
  /// with the given fields replaced by the non-null parameter values.
  @JsonKey(includeFromJson: false, includeToJson: false)
  @pragma('vm:prefer-inline')
  $ChatEvent_PeerChangedCopyWith<ChatEvent_PeerChanged> get copyWith =>
      _$ChatEvent_PeerChangedCopyWithImpl<ChatEvent_PeerChanged>(
        this,
        _$identity,
      );

  @override
  bool operator ==(Object other) {
    return identical(this, other) ||
        (other.runtimeType == runtimeType &&
            other is ChatEvent_PeerChanged &&
            (identical(other.revision, revision) ||
                other.revision == revision) &&
            (identical(other.peer, peer) || other.peer == peer));
  }

  @override
  int get hashCode => Object.hash(runtimeType, revision, peer);

  @override
  String toString() {
    return 'ChatEvent.peerChanged(revision: $revision, peer: $peer)';
  }
}

/// @nodoc
abstract mixin class $ChatEvent_PeerChangedCopyWith<$Res>
    implements $ChatEventCopyWith<$Res> {
  factory $ChatEvent_PeerChangedCopyWith(
    ChatEvent_PeerChanged value,
    $Res Function(ChatEvent_PeerChanged) _then,
  ) = _$ChatEvent_PeerChangedCopyWithImpl;
  @useResult
  $Res call({BigInt revision, PeerInfo peer});
}

/// @nodoc
class _$ChatEvent_PeerChangedCopyWithImpl<$Res>
    implements $ChatEvent_PeerChangedCopyWith<$Res> {
  _$ChatEvent_PeerChangedCopyWithImpl(this._self, this._then);

  final ChatEvent_PeerChanged _self;
  final $Res Function(ChatEvent_PeerChanged) _then;

  /// Create a copy of ChatEvent
  /// with the given fields replaced by the non-null parameter values.
  @pragma('vm:prefer-inline')
  $Res call({Object? revision = null, Object? peer = null}) {
    return _then(
      ChatEvent_PeerChanged(
        revision: null == revision
            ? _self.revision
            : revision // ignore: cast_nullable_to_non_nullable
                  as BigInt,
        peer: null == peer
            ? _self.peer
            : peer // ignore: cast_nullable_to_non_nullable
                  as PeerInfo,
      ),
    );
  }
}

/// @nodoc

class ChatEvent_ProbeTextReceived extends ChatEvent {
  const ChatEvent_ProbeTextReceived({required this.peerId, required this.text})
    : super._();

  final String peerId;
  final String text;

  /// Create a copy of ChatEvent
  /// with the given fields replaced by the non-null parameter values.
  @JsonKey(includeFromJson: false, includeToJson: false)
  @pragma('vm:prefer-inline')
  $ChatEvent_ProbeTextReceivedCopyWith<ChatEvent_ProbeTextReceived>
  get copyWith =>
      _$ChatEvent_ProbeTextReceivedCopyWithImpl<ChatEvent_ProbeTextReceived>(
        this,
        _$identity,
      );

  @override
  bool operator ==(Object other) {
    return identical(this, other) ||
        (other.runtimeType == runtimeType &&
            other is ChatEvent_ProbeTextReceived &&
            (identical(other.peerId, peerId) || other.peerId == peerId) &&
            (identical(other.text, text) || other.text == text));
  }

  @override
  int get hashCode => Object.hash(runtimeType, peerId, text);

  @override
  String toString() {
    return 'ChatEvent.probeTextReceived(peerId: $peerId, text: $text)';
  }
}

/// @nodoc
abstract mixin class $ChatEvent_ProbeTextReceivedCopyWith<$Res>
    implements $ChatEventCopyWith<$Res> {
  factory $ChatEvent_ProbeTextReceivedCopyWith(
    ChatEvent_ProbeTextReceived value,
    $Res Function(ChatEvent_ProbeTextReceived) _then,
  ) = _$ChatEvent_ProbeTextReceivedCopyWithImpl;
  @useResult
  $Res call({String peerId, String text});
}

/// @nodoc
class _$ChatEvent_ProbeTextReceivedCopyWithImpl<$Res>
    implements $ChatEvent_ProbeTextReceivedCopyWith<$Res> {
  _$ChatEvent_ProbeTextReceivedCopyWithImpl(this._self, this._then);

  final ChatEvent_ProbeTextReceived _self;
  final $Res Function(ChatEvent_ProbeTextReceived) _then;

  /// Create a copy of ChatEvent
  /// with the given fields replaced by the non-null parameter values.
  @pragma('vm:prefer-inline')
  $Res call({Object? peerId = null, Object? text = null}) {
    return _then(
      ChatEvent_ProbeTextReceived(
        peerId: null == peerId
            ? _self.peerId
            : peerId // ignore: cast_nullable_to_non_nullable
                  as String,
        text: null == text
            ? _self.text
            : text // ignore: cast_nullable_to_non_nullable
                  as String,
      ),
    );
  }
}
